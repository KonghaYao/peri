//! Interactive storybook for `peri-tui-components`.
//!
//! Run with `./dev-tui.sh`, or directly:
//! `cargo run -p peri-tui-components --bin storybook`.
//!
//! Mouse is the primary input; keys stay available as a convenience.

mod app;
mod charts;
mod stories;

use std::io;

use crate::stories::STORIES;

fn list_stories() {
    for story in STORIES {
        for variant in story.variants {
            println!("{}\t{}", story.key, variant.key);
        }
    }
}

fn story_arg(args: &[String]) -> Option<&str> {
    args.iter()
        .position(|arg| arg == "--story")
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn variant_arg(args: &[String]) -> Option<&str> {
    args.iter()
        .position(|arg| arg == "--variant")
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn size_arg(args: &[String], flag: &str) -> Option<u16> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
}

fn print_help() {
    println!(
        "peri-tui storybook\n\n\
         USAGE:\n    storybook [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20   --story <key>   start on a story\n\
         \x20   --variant <key> start on a variant\n\
         \x20   --list          list story/variant keys\n\
         \x20   --dump          render one screen to stdout (default 100x32)\n\
         \x20   --dump-all      render every story and variant to stdout\n\
         \x20   --width <n>     dump width (default 100)\n\
         \x20   --height <n>    dump height (default 32)\n\
         \x20   -h, --help      show this help\n\n\
         MOUSE:\n\
         \x20   click story / variant   switch views\n\
         \x20   click preview           use the component (caret, selection)\n\
         \x20   wheel over preview      scroll list / scrollbar / document\n\n\
         KEYS:\n\
         \x20   q/Esc/Ctrl-C   quit (other keys edit the focused TextArea)\n\
         \n\
         Navigation is mouse-only, like the host TUI."
    );
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--list") {
        list_stories();
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return Ok(());
    }
    let width = size_arg(&args, "--width").unwrap_or(100);
    let height = size_arg(&args, "--height").unwrap_or(32);
    if args.iter().any(|arg| arg == "--dump-all") {
        app::dump_all(width, height);
        return Ok(());
    }
    let story = match story_arg(&args) {
        Some(value) => match STORIES
            .iter()
            .position(|story| story.key.eq_ignore_ascii_case(value))
        {
            Some(index) => index,
            None => {
                eprintln!("unknown story: {value}");
                list_stories();
                return Ok(());
            }
        },
        None => 0,
    };
    let variant = match variant_arg(&args) {
        Some(value) => match STORIES[story]
            .variants
            .iter()
            .position(|variant| variant.key.eq_ignore_ascii_case(value))
        {
            Some(index) => index,
            None => {
                eprintln!("unknown variant: {value}");
                list_stories();
                return Ok(());
            }
        },
        None => 0,
    };
    if args.iter().any(|arg| arg == "--dump") {
        app::dump_one(story, variant, width, height);
        return Ok(());
    }
    app::run(story, variant)
}
