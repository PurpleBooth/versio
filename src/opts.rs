//! The command-line options for the sorcery executable.

use crate::analyze::analyze;
use crate::config::{Config, ShowFormat};
use crate::error::Result;
use crate::git::pull_ff_only;
use crate::{CurrentSource, PrevSource, Source};
use clap::{crate_version, App, AppSettings, Arg, ArgGroup, ArgMatches, SubCommand};
use git2::Repository;

pub fn execute() -> Result<()> {
  let m = App::new("versio")
    .setting(AppSettings::UnifiedHelpMessage)
    .author("Charlie Ozinga, charlie@cloud-elements.com")
    .version(concat!(crate_version!(), " (", env!("GIT_SHORT_HASH"), ")"))
    .about("Manage version numbers")
    .subcommand(
      SubCommand::with_name("pull").setting(AppSettings::UnifiedHelpMessage).about("Pull the repo").display_order(1)
    )
    .subcommand(
      SubCommand::with_name("check")
        .setting(AppSettings::UnifiedHelpMessage)
        .about("Check current config")
        .display_order(1)
    )
    .subcommand(
      SubCommand::with_name("show")
        .setting(AppSettings::UnifiedHelpMessage)
        .about("Show all versions")
        .arg(
          Arg::with_name("prev")
            .short("p")
            .long("prev")
            .takes_value(false)
            .display_order(1)
            .help("Whether to show prev versions")
        )
        .arg(
          Arg::with_name("wide")
            .short("w")
            .long("wide")
            .takes_value(false)
            .display_order(1)
            .help("Wide output shows IDs")
        )
        .arg(
          Arg::with_name("nofetch").short("F").long("no-fetch").takes_value(false).display_order(1).help("Don't fetch")
        )
        .display_order(1)
    )
    .subcommand(
      SubCommand::with_name("get")
        .setting(AppSettings::UnifiedHelpMessage)
        .about("Show one or more versions")
        .arg(
          Arg::with_name("prev")
            .short("p")
            .long("prev")
            .takes_value(false)
            .display_order(1)
            .help("Whether to show prev versions")
        )
        .arg(
          Arg::with_name("nofetch").short("F").long("no-fetch").takes_value(false).display_order(1).help("Don't fetch")
        )
        .arg(
          Arg::with_name("versiononly")
            .short("v")
            .long("version-only")
            .takes_value(false)
            .display_order(1)
            .help("Only show the version number")
        )
        .arg(
          Arg::with_name("wide")
            .short("w")
            .long("wide")
            .takes_value(false)
            .display_order(1)
            .help("Wide output shows IDs")
        )
        .arg(
          Arg::with_name("name")
            .short("n")
            .long("name")
            .takes_value(true)
            .value_name("name")
            .display_order(1)
            .help("The name to get")
        )
        .arg(
          Arg::with_name("id")
            .short("i")
            .long("id")
            .takes_value(true)
            .value_name("id")
            .display_order(1)
            .help("The id to get")
        )
        .group(ArgGroup::with_name("ident").args(&["id", "name"]).required(true))
        .display_order(1)
    )
    .subcommand(
      SubCommand::with_name("set")
        .setting(AppSettings::UnifiedHelpMessage)
        .about("Set a version")
        .arg(
          Arg::with_name("name")
            .short("n")
            .long("name")
            .takes_value(true)
            .value_name("name")
            .display_order(1)
            .help("The name to set")
        )
        .arg(
          Arg::with_name("id")
            .short("i")
            .long("id")
            .takes_value(true)
            .value_name("id")
            .display_order(1)
            .help("The id to set")
        )
        .group(ArgGroup::with_name("ident").args(&["id", "name"]).required(true))
        .arg(
          Arg::with_name("value")
            .short("v")
            .long("value")
            .takes_value(true)
            .value_name("value")
            .display_order(2)
            .required(true)
            .help("The value to set to")
        )
        .display_order(1)
    )
    .subcommand(
      SubCommand::with_name("diff")
        .setting(AppSettings::UnifiedHelpMessage)
        .about("Set a version")
        .arg(
          Arg::with_name("nofetch").short("F").long("no-fetch").takes_value(false).display_order(1).help("Don't fetch")
        )
        .display_order(1)
    )
    .subcommand(
      SubCommand::with_name("files")
        .setting(AppSettings::UnifiedHelpMessage)
        .about("Set a version")
        .arg(
          Arg::with_name("nofetch").short("F").long("no-fetch").takes_value(false).display_order(1).help("Don't fetch")
        )
        .display_order(1)
    )
    .subcommand(
      SubCommand::with_name("plan")
        .setting(AppSettings::UnifiedHelpMessage)
        .about("Find versions that need to change")
        .arg(
          Arg::with_name("nofetch").short("F").long("no-fetch").takes_value(false).display_order(1).help("Don't fetch")
        )
        .display_order(1)
    )
    .get_matches();

  parse_matches(m)
}

fn parse_matches(m: ArgMatches) -> Result<()> {
  let mut prev = PrevSource::open(".")?;
  let curt = CurrentSource::open(".")?;

  match m.subcommand() {
    ("check", _) => check(curt),
    ("show", Some(m)) => {
      if m.is_present("nofetch") {
        prev.set_fetch(false)?;
      }
      let fmt = ShowFormat::new(m.is_present("wide"), false);
      if m.is_present("prev") {
        show(prev, fmt)
      } else {
        show(curt, fmt)
      }
    }
    ("get", Some(m)) => {
      if m.is_present("nofetch") {
        prev.set_fetch(false)?;
      }
      let fmt = ShowFormat::new(m.is_present("wide"), m.is_present("versiononly"));
      if m.is_present("prev") {
        if m.is_present("id") {
          get_id(prev, m.value_of("id").unwrap(), fmt)
        } else {
          get_name(prev, m.value_of("name").unwrap(), fmt)
        }
      } else if m.is_present("id") {
        get_id(curt, m.value_of("id").unwrap(), fmt)
      } else {
        get_name(curt, m.value_of("name").unwrap(), fmt)
      }
    }
    ("diff", Some(m)) => {
      if m.is_present("nofetch") {
        prev.set_fetch(false)?;
      }
      diff(prev, curt)
    }
    ("files", Some(m)) => {
      if m.is_present("nofetch") {
        prev.set_fetch(false)?;
      }
      for result in prev.repo()?.get_keyed_files()? {
        let (key, path) = result?;
        println!("{} : {}", key, path);
      }
      Ok(())
    }
    ("plan", Some(m)) => {
      if m.is_present("nofetch") {
        prev.set_fetch(false)?;
      }
      plan(prev, curt)
    }
    ("set", Some(m)) => {
      if m.is_present("id") {
        set_by_id(m.value_of("id").unwrap(), m.value_of("value").unwrap())
      } else {
        set_by_name(m.value_of("name").unwrap(), m.value_of("value").unwrap())
      }
    }
    ("pull", _) => pull_ff_only(&Repository::open(".")?, None, None),
    ("", _) => empty_cmd(),
    (c, _) => unknown_cmd(c)
  }
}

fn diff(prev: PrevSource, curt: CurrentSource) -> Result<()> {
  let prev_at = Config::from_source(prev)?.annotate()?;
  let curt_at = Config::from_source(curt)?.annotate()?;

  let analysis = analyze(&prev_at, &curt_at);

  if !analysis.older().is_empty() {
    println!("Removed projects:");
    for mark in analysis.older() {
      println!("  {} : {}", mark.name(), mark.mark().value());
    }
    println!();
  }

  if !analysis.newer().is_empty() {
    println!("New projects:");
    for mark in analysis.newer() {
      println!("  {} : {}", mark.name(), mark.mark().value());
    }
    println!();
  }

  if analysis.changes().iter().any(|c| c.value().is_some()) {
    println!("Changed versions:");
    for change in analysis.changes().iter().filter(|c| c.value().is_some()) {
      print!("  {}", change.new_mark().name());

      if let Some((o, _)) = change.name().as_ref() {
        print!(" (was \"{}\")", o);
      }
      if let Some((o, n)) = change.value().as_ref() {
        print!(" : {} -> {}", o, n);
      } else {
        print!(" : {}", change.new_mark().mark().value());
      }
      println!();
    }
    println!();
  }

  if analysis.changes().iter().any(|c| c.value().is_none()) {
    println!("Unchanged versions:");
    for change in analysis.changes().iter().filter(|c| c.value().is_none()) {
      print!("  {}", change.new_mark().name());

      if let Some((o, _)) = change.name().as_ref() {
        print!(" (was \"{}\")", o);
      }
      print!(" : {}", change.new_mark().mark().value());
      println!();
    }
    println!();
  }

  Ok(())
}

pub fn plan(prev: PrevSource, cur: CurrentSource) -> Result<()> {
  let config = Config::from_source(cur)?;
  let mut plan = config.plan();

  for result in prev.repo()?.get_keyed_files()? {
    let (key, path) = result?;
    plan.consider(&key, &path)?;
  }
  plan.consider_deps()?;

  for (id, size) in plan.sorted_incrs() {
    println!("{} : {}", config.get_project(id).unwrap().name(), size);
  }

  Ok(())
}

fn check(curt: CurrentSource) -> Result<()> { Config::from_source(curt)?.check() }

fn show<S: Source>(source: S, fmt: ShowFormat) -> Result<()> { Config::from_source(source)?.show(fmt) }

fn current_config() -> Result<Config<CurrentSource>> { Config::from_source(CurrentSource::open(".")?) }

fn get_name<S: Source>(src: S, name: &str, fmt: ShowFormat) -> Result<()> {
  Config::from_source(src)?.show_names(name, fmt)
}

fn get_id<S: Source>(src: S, id: &str, fmt: ShowFormat) -> Result<()> {
  Config::from_source(src)?.show_id(id.parse()?, fmt)
}

fn set_by_name(name: &str, val: &str) -> Result<()> { current_config()?.set_by_name(name, val) }

fn set_by_id(id: &str, val: &str) -> Result<()> { current_config()?.set_by_id(id.parse()?, val) }

fn unknown_cmd(c: &str) -> Result<()> { versio_err!("Unknown command: \"{}\" (try \"help\").", c) }

fn empty_cmd() -> Result<()> { versio_err!("No command (try \"help\").") }
