use std::fs::File;
use std::io::prelude::*;
use std::{fmt::Display, io};

use base64::{Engine as _, engine::general_purpose};
use eyre::{eyre, Result};
use html_escape::decode_html_entities;
use regex::Regex;
use ureq::serde_json::{self, Value};
use url::Url;

use clap::Parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// URL of the vimeo event
    #[clap(short, long)]
    url: String,
    /// Referer
    #[clap(short, long)]
    referer: String,
    /// output filename
    #[clap(short, long)]
    filename: String,
}

fn main() {
    let args = Args::parse();
    let agent = ureq::agent();

    let config_url = get_config_url(&agent, &args.url, &args.referer).unwrap();
    let master_url = get_master_url(&agent, &config_url).unwrap();
    let videos = get_video_infos(&master_url).unwrap();
    println!("Found {} videos", videos.len());
    for video in &videos {
        println!("{}", video);
    }
    let video = videos.iter().max_by_key(|v| v.width).unwrap();
    println!("Found best video: {}", &video);

    download(&args.filename, video).unwrap();
}

fn get_config_url(agent: &ureq::Agent, url: &str, referer: &str) -> Result<String> {
    let result = agent
        .get(url)
        .set("Referer", referer)
        .call()?
        .into_string()?;

    let re = Regex::new(r##"data-config-url="([^"]+)""##).unwrap();
    let captures = re
        .captures(&result)
        .ok_or(eyre!("Did not find video config url!"))?;
    captures
        .get(1)
        .map(|m| decode_html_entities(m.as_str()).into_owned())
        .ok_or(eyre!("Invalid capture group!"))
}

fn get_master_url(agent: &ureq::Agent, config_url: &str) -> Result<String> {
    let result: serde_json::Value = agent.get(config_url).call()?.into_json()?;
    let dash_config = &result["request"]["files"]["dash"];
    let default_cdn = &dash_config["default_cdn"].as_str().unwrap();
    let cdns = &dash_config["cdns"];
    let cdn_config = &cdns[&default_cdn];
    Ok((&cdn_config["url"]).as_str().unwrap().to_string())
}

struct VideoInfo {
    base_url: String,
    id: String,
    codecs: String,
    bitrate: u64,
    duration: f64,
    width: u64,
    height: u64,
    init_segment: Vec<u8>,
    segments: Vec<Segment>,
}

struct Segment {
    path: String,
    size: u64,
}

impl Display for VideoInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}, {}x{}, {} seconds, {} bitrate",
            self.id, self.codecs, self.width, self.height, self.duration, self.bitrate
        )
    }
}

fn get_video_infos(master_url: &str) -> Result<Vec<VideoInfo>> {
    let result: serde_json::Value = ureq::get(master_url).call()?.into_json()?;
    let base_url = &result["base_url"].as_str().unwrap();
    let base_url = Url::parse(master_url).unwrap().join(base_url)?;
    let videos = result["video"].as_array().unwrap();

    let videos: Vec<_> = videos
        .iter()
        .map(|v| extract_video_info(v, &base_url))
        .collect();

    Ok(videos)
}

fn extract_video_info(value: &Value, base_url: &Url) -> VideoInfo {
    let init_segment = value["init_segment"].as_str().unwrap();

    VideoInfo {
        base_url: base_url.to_string(),
        id: value["id"].to_string(),
        codecs: value["codecs"].to_string(),
        bitrate: value["bitrate"].as_u64().unwrap(),
        duration: value["duration"].as_f64().unwrap(),
        width: value["width"].as_u64().unwrap(),
        height: value["height"].as_u64().unwrap(),
        init_segment: general_purpose::STANDARD_NO_PAD.decode(init_segment).unwrap(),
        segments: value["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| Segment {
                path: s["url"].as_str().unwrap().to_string(),
                size: s["size"].as_u64().unwrap(),
            })
            .collect(),
    }
}

fn download(file_path: &str, video: &VideoInfo) -> Result<()> {
    let agent = ureq::agent();
    let mut file = File::create(file_path)?;
    file.write_all(&video.init_segment)?;
    let url = Url::parse(&video.base_url)?;
    let sum: u64 = video.segments.iter().map(|s| s.size).sum();
    let bar = indicatif::ProgressBar::new(sum);

    for segment in video.segments.iter() {
        let url = url.join(&segment.path)?;
        let mut reader = agent.get(url.as_str()).call()?.into_reader();
        let count = io::copy(&mut reader, &mut file)?;
        if count != segment.size + 1 {
            let size = segment.size;
            return Err(eyre!(format!(
                "Invalid byte count! Read={count}, expected={size}"
            )));
        }
        bar.inc(count - 1);
    }

    bar.finish();

    Ok(())
}
