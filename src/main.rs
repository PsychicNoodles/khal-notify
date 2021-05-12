#![feature(iter_intersperse)]

use clap::{App, Arg};
use regex::Regex;
use serde::Deserialize;
use std::path::Path;
use std::process::Command;
use std::{array, time::Duration};
use std::{sync::Arc, thread};
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};
use unicode_segmentation::UnicodeSegmentation;

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

    fn formatted_title(&self) -> String {
        if self.repeat_symbol.is_empty() {
            self.title.clone()
        } else {
            self.title.clone() + " " + &self.repeat_symbol
        }
    }
}

pub fn main() {
    let config_default = directories::BaseDirs::new()
        .map(|d| d.config_dir().join(Path::new("khal/config")))
        .map(|pb| pb.to_str().map(str::to_owned))
        .flatten()
        .unwrap_or_else(|| "khal.conf".to_owned());
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
            Arg::with_name("strip regex")
                .short("s")
                .long("strip-regex")
                .value_name("REGEX")
                .multiple(true)
                .number_of_values(1)
                .allow_hyphen_values(true)
                .help("regex for text to strip from event descriptions"),
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
    let strip_regexes = Arc::new(
        matches
            .values_of("strip regex")
            .map(|i| i.map(Regex::new).flatten().collect())
            .unwrap_or_else(|| Vec::new()),
    );

    let url_regex = Arc::new(Regex::new(URL_REGEX).unwrap());

    let target = if at.contains(':') || at.contains(' ') {
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
            "--json",
        ])
        .args(array::IntoIter::new(JSON_FIELDS).intersperse("--json"))
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
        let strip_regexes = Arc::clone(&strip_regexes);
        let url_regex = Arc::clone(&url_regex);
        let handle = thread::spawn(move || {
            let title = event.formatted_title();

            let stripped_desc = strip_regexes
                .iter()
                .fold(event.description.clone(), |d, regex| {
                    regex.replace_all(&d, "").into_owned()
                });
            let mut short_desc = if desc_chars < stripped_desc.len() {
                let mut desc_graphemes = stripped_desc.graphemes(true);
                let mut short_desc =
                    desc_graphemes.by_ref().take(desc_chars).collect::<String>() + "...";
                for link in find_links(
                    url_regex,
                    desc_graphemes.by_ref().skip(desc_chars).collect(),
                ) {
                    short_desc += &link
                }
                short_desc
            } else {
                stripped_desc
            };
            if !event.all_day {
                if !short_desc.ends_with('\n') {
                    short_desc += "\n";
                }
                short_desc += &event.start_end_time_style;
            }

            Command::new("notify-send")
                .args(&[title, short_desc])
                .spawn()
                .expect("could not create notification")
                .wait()
                .expect("notification process ended unexpectedly");
        });
        handles.push(handle);
    }
    handles
        .into_iter()
        .for_each(|handle| handle.join().expect("failed to join notify thread"));
}

fn find_links(url_regex: Arc<Regex>, rem_desc: String) -> Vec<String> {
    let urls: Vec<_> = url_regex.captures_iter(&rem_desc).collect();
    let mut url_matches: Vec<_> = urls
        .iter()
        .map(|cap| cap.get(0))
        .flatten()
        .map(|url| url.as_str())
        .collect();
    url_matches.sort_unstable();
    url_matches.dedup();
    url_matches
        .iter()
        .map(|url| format!("<a href=\"{}\"></a>", url))
        .collect()
}
