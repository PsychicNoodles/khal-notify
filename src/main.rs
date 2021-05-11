#![feature(iter_intersperse)]

use clap::{App, Arg};
use regex::Regex;
use serde::Deserialize;
use std::process::Command;
use std::thread;
use std::{array, time::Duration};
use std::{iter, path::Path};
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};

const MINUTE_OFFSET: &str = "10";
const DESC_CHARS: &str = "200";

const JSON_FIELDS: [&str; 5] = [
    "title",
    "description",
    "start-end-time-style",
    "repeat-symbol",
    "all-day",
];
const URL_REGEX: &str = r"(https?://(www\.)?)?[-a-zA-Z0-9@:%._\+~#=]{1,256}\.[a-zA-Z0-9()]{1,6}\b([-a-zA-Z0-9()@:%_\+.~#?&//=]*)";

#[derive(Deserialize, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
struct KhalEvent {
    title: String,
    description: String,
    start_end_time_style: String,
    repeat_symbol: String,
    all_day: bool,
}

impl KhalEvent {
    fn is_all_day(&self) -> bool {
        self.all_day
    }
}

pub fn main() {
    let config_default = directories::BaseDirs::new()
        .map(|d| d.config_dir().join(Path::new("khal/config")))
        .map(|pb| pb.to_str().map(str::to_owned))
        .flatten()
        .unwrap_or("khal.conf".to_owned());
    let matches = App::new("khal-notify")
        .version("1.0")
        .author("Mattori Birnbaum <mattori.birnbaum@gmail.com>")
        .about("Checks khal and sends notifications for upcoming events")
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .value_name("FILE")
                .help("khal config location")
                .default_value(&config_default),
        )
        .arg(
            Arg::with_name("description length")
                .short("l")
                .long("desc-length")
                .value_name("CHARS")
                .help("character limit for event description")
                .default_value(DESC_CHARS),
        )
        .arg(
            Arg::with_name("include all day")
                .short("a")
                .long("all-day")
                .help("include all day events"),
        )
        .arg(
            Arg::with_name("date format")
                .short("d")
                .long("date-format")
                .value_name("FORMAT")
                .help("date format expected by khal")
                .default_value("%F"),
        )
        .arg(
            Arg::with_name("time format")
                .short("t")
                .long("time-format")
                .value_name("FORMAT")
                .help("time format expected by khal")
                .default_value("%R"),
        )
        .arg(
            Arg::with_name("utc offset")
                .short("z")
                .long("timezone")
                .value_name("HOURS")
                .help("utc offset of local timezone")
                .default_value("+9"),
        )
        .arg(
            Arg::with_name("AT")
                .value_name("TIME")
                .multiple(true)
                .help("minutes in the future or datetime (YYYY-mm-dd HH:MM) to check for events")
                .default_value(MINUTE_OFFSET),
        )
        .get_matches();

    let config = matches.value_of("config").unwrap();
    let at: String = matches
        .values_of("AT")
        .unwrap()
        .into_iter()
        .intersperse(" ")
        .collect();
    let desc_chars = matches
        .value_of("description length")
        .unwrap()
        .parse()
        .expect("description length is not a number");
    let include_all_day = matches.is_present("include all day");
    let date_format = matches.value_of("date format").unwrap();
    let time_format = matches.value_of("time format").unwrap();
    let utc_offset = UtcOffset::hours(
        matches
            .value_of("utc offset")
            .unwrap()
            .parse::<i8>()
            .expect("utc offset of unexpected format"),
    );

    let target = if at.contains(":") || at.contains(" ") {
        PrimitiveDateTime::parse(at, "%F %R")
            .expect("datetime offset of unexpected format")
            .assume_offset(utc_offset)
    } else {
        let offset_duration =
            Duration::from_secs(at.parse::<u64>().expect("offset is not a number") * 60);
        OffsetDateTime::now_utc().to_offset(utc_offset) + offset_duration
    };

    let khal_output = Command::new("khal")
        .args(&[
            "--config",
            config,
            "at",
            &target.format(date_format),
            &target.format(time_format),
            "--notstarted",
        ])
        .args(iter::once("--json").chain(array::IntoIter::new(JSON_FIELDS).intersperse("--json")))
        .output()
        .expect("could not execute khal")
        .stdout;

    let mut events: Vec<KhalEvent> =
        serde_json::from_slice(&khal_output).expect("khal output of unexpected format");

    if !include_all_day {
        events = events.into_iter().filter(|e| !e.is_all_day()).collect();
    }

    let mut handles = Vec::with_capacity(events.len());
    for event in events {
        let handle = thread::spawn(move || {
            let mut title =
                String::with_capacity(event.title.len() + event.repeat_symbol.len() + 1);
            title += &event.title;
            if !event.repeat_symbol.is_empty() {
                title += " ";
                title += &event.repeat_symbol;
            }

            let url_regex = Regex::new(URL_REGEX).unwrap();
            let urls: Vec<_> = url_regex.captures_iter(&event.description).collect();
            let mut short_desc = String::with_capacity(desc_chars + 100);
            short_desc += &event.description[..std::cmp::min(desc_chars, event.description.len())];
            if event.description.len() > desc_chars {
                short_desc += "...";
            }
            if !event.all_day {
                if !short_desc.ends_with("\n") {
                    short_desc += "\n";
                }
                short_desc += &event.start_end_time_style;
            }

            let mut url_matches: Vec<_> = urls
                .iter()
                .map(|cap| cap.get(0))
                .flatten()
                .map(|url| url.as_str())
                .collect();
            url_matches.sort();
            url_matches.dedup();

            let actions = if urls.is_empty() {
                Vec::new()
            } else {
                iter::once("--action".to_owned())
                    .chain(
                        url_matches
                            .iter()
                            .enumerate()
                            .map(|(i, url)| format!("{}:{}", i, url)),
                    )
                    .chain(iter::once("--".to_owned()))
                    .collect()
            };

            let notify_output = Command::new("notify-send.py")
                .args(actions.iter())
                .args(&[title, short_desc])
                .output()
                .expect("could not create notification")
                .stdout;

            if !urls.is_empty() {
                let action_result = std::str::from_utf8(&notify_output)
                    .expect("notify-send output of non-text format")
                    .lines()
                    .nth(0)
                    .expect("notify-send output of unexpected format");
                if action_result != "closed" {
                    let chosen_action_index = action_result
                        .parse::<usize>()
                        .expect("notify-send output of non-numeric format");
                    let chosen_action = url_matches
                        .get(chosen_action_index)
                        .expect("notify-send returned invalid action");

                    opener::open(chosen_action).expect("could not open chosen action");
                }
            }
        });
        handles.push(handle);
    }
    handles
        .into_iter()
        .for_each(|handle| handle.join().expect("failed to join notify thread"));
}
