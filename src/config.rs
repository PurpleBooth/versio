//! The configuration and top-level commands for Versio.

use crate::analyze::AnnotatedMark;
use crate::error::Result;
use crate::git::{CommitData, FullPr};
use crate::scan::{parts::{deserialize_parts, Part}, JsonScanner, Scanner, TomlScanner, YamlScanner};
use crate::{CurrentSource, Mark, MarkedData, NamedData, PrevSource, Source, CONFIG_FILENAME};
use glob::{glob_with, MatchOptions, Pattern};
use regex::Regex;
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use std::borrow::Cow;
use std::cmp::{max, Ord, Ordering};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::Path;

pub fn configure_plan<'s>(
  prev: &'s PrevSource, curt: &'s CurrentSource
) -> Result<(Plan, Config<&'s PrevSource>, Config<&'s CurrentSource>)> {
  let prev_config = Config::from_source(prev)?;
  let curt_config = Config::from_source(curt)?;
  let mut plan = prev_config.start_plan(&curt_config);

  for pr in prev.changes()?.groups().values() {
    plan.consider_pr(pr)?;
    for commit in pr.included_commits() {
      plan.consider_commit(commit.clone())?;
      for file in commit.files() {
        plan.consider_file(file)?;
        plan.finish_file()?;
      }
      plan.finish_commit()?;
    }
    plan.finish_pr()?;
  }

  plan.consider_deps()?;

  let plan = plan.finish_plan()?;
  Ok((plan, prev_config, curt_config))
}

pub struct ShowFormat {
  pub wide: bool,
  pub version_only: bool
}

impl ShowFormat {
  pub fn new(wide: bool, version_only: bool) -> ShowFormat { ShowFormat { wide, version_only } }
}

pub struct Config<S: Source> {
  source: S,
  file: ConfigFile
}

impl<'s> Config<&'s PrevSource> {
  fn start_plan<C: Source>(&'s self, current: &'s Config<C>) -> PlanConsider<C> {
    PlanConsider::new(self, current)
  }
}

impl<S: Source> Config<S> {
  pub fn has_config_file(source: S) -> Result<bool> { source.has(CONFIG_FILENAME.as_ref()) }

  pub fn from_source(source: S) -> Result<Config<S>> {
    let file = ConfigFile::load(&source)?;
    Ok(Config { source, file })
  }

  pub fn annotate(&self) -> Result<Vec<AnnotatedMark>> {
    self.file.projects.iter().map(|p| p.annotate(&self.source)).collect()
  }

  pub fn check(&self) -> Result<()> {
    for project in &self.file.projects {
      project.check(&self.source)?;
    }
    Ok(())
  }

  pub fn get_mark(&self, id: u32) -> Option<Result<MarkedData>> {
    self.get_project(id).map(|p| p.get_mark(&self.source))
  }

  pub fn show(&self, format: ShowFormat) -> Result<()> {
    let name_width = self.file.projects.iter().map(|p| p.name.len()).max().unwrap_or(0);

    for project in &self.file.projects {
      project.show(&self.source, name_width, &format)?;
    }
    Ok(())
  }

  pub fn get_project(&self, id: u32) -> Option<&Project> { self.file.projects.iter().find(|p| p.id == id) }

  pub fn show_id(&self, id: u32, format: ShowFormat) -> Result<()> {
    let project = self.get_project(id).ok_or_else(|| versio_error!("No such project {}", id))?;
    project.show(&self.source, 0, &format)
  }

  pub fn show_names(&self, name: &str, format: ShowFormat) -> Result<()> {
    let filter = |p: &&Project| p.name.contains(name);
    let name_width = self.file.projects.iter().filter(filter).map(|p| p.name.len()).max().unwrap_or(0);

    for project in self.file.projects.iter().filter(filter) {
      project.show(&self.source, name_width, &format)?;
    }
    Ok(())
  }

  pub fn set_by_name(&self, name: &str, val: &str) -> Result<()> {
    let id = self.find_unique(name)?;
    self.set_by_id(id, val)
  }

  pub fn set_by_id(&self, id: u32, val: &str) -> Result<()> {
    let project =
      self.file.projects.iter().find(|p| p.id == id).ok_or_else(|| versio_error!("No such project {}", id))?;
    project.set_value(&self.source, val)
  }

  fn find_unique(&self, name: &str) -> Result<u32> {
    let mut iter = self.file.projects.iter().filter(|p| p.name.contains(name)).map(|p| p.id);
    let id = iter.next().ok_or_else(|| versio_error!("No project named {}", name))?;
    if iter.next().is_some() {
      return versio_err!("Multiple projects with name {}", name);
    }
    Ok(id)
  }
}

pub struct Plan {
  incrs: HashMap<u32, (Size, ChangeLog)>,   // proj ID, incr size, change log
  ineffective: Vec<SizedPr>                 // PRs that didn't apply to any project
}

impl Plan {
  pub fn incrs(&self) -> &HashMap<u32, (Size, ChangeLog)> { &self.incrs }
  pub fn ineffective(&self) -> &[SizedPr] { &self.ineffective }
}

pub struct ChangeLog {
  entries: Vec<(SizedPr, Size)>
}

impl ChangeLog {
  pub fn empty() -> ChangeLog { ChangeLog { entries: Vec::new() } }
}

impl ChangeLog {
  pub fn add_entry(&mut self, pr: SizedPr, size: Size) {
    self.entries.push((pr, size));
  }
}

pub struct SizedPr {
  number: u32,
  commits: Vec<(String, String, Size, bool)>    // oid, message, size, applies to this project
}

impl SizedPr {
  pub fn empty(number: u32) -> SizedPr { SizedPr { number, commits: Vec::new() } }
  pub fn number(&self) -> u32 { self.number }
  pub fn commits(&self) -> &[(String, String, Size, bool)] { &self.commits }
}

pub struct PlanConsider<'s, C: Source> {
  on_pr_sizes: HashMap<u32, SizedPr>,
  on_ineffective: Option<SizedPr>,
  on_commit: Option<CommitData>,
  prev: OnPrev<'s>,
  current: &'s Config<C>,
  incrs: HashMap<u32, (Size, ChangeLog)>,   // proj ID, incr size, change log
  ineffective: Vec<SizedPr>                 // PRs that didn't apply to any project
}

impl<'s, C: Source> PlanConsider<'s, C> {
  fn new(prev: &'s Config<&'s PrevSource>, current: &'s Config<C>) -> PlanConsider<'s, C> {
    let prev = OnPrev::Initial(prev);
    PlanConsider {
      on_pr_sizes: HashMap::new(),
      on_ineffective: None,
      on_commit: None,
      prev,
      current,
      incrs: HashMap::new(),
      ineffective: Vec::new()
    }
  }

  pub fn consider_pr(&mut self, pr: &FullPr) -> Result<()> {
    self.on_pr_sizes = self.current.file.projects.iter().map(|p| (p.id(), SizedPr::empty(pr.number()))).collect();
    self.on_ineffective = Some(SizedPr::empty(pr.number()));
    Ok(())
  }

  pub fn finish_pr(&mut self) -> Result<()> {
    let mut found = false;
    for (proj_id, sized_pr) in self.on_pr_sizes.drain() {
      let (size, change_log) = self.incrs.entry(proj_id).or_insert((Size::None, ChangeLog::empty()));
      let pr_size = sized_pr.commits.iter().filter(|(_, _, _, appl)| *appl).map(|(_, _, sz, _)| sz).max().cloned();
      if let Some(pr_size) = pr_size {
        found = true;
        *size = max(*size, pr_size);
        change_log.add_entry(sized_pr, pr_size);
      }
    }

    let ineffective = self.on_ineffective.take().unwrap();
    if !found {
      self.ineffective.push(ineffective);
    }

    Ok(())
  }

  pub fn consider_commit(&mut self, commit: CommitData) -> Result<()> {
    let id = commit.id().to_string();
    let kind = commit.kind().to_string();
    let summary = commit.summary().to_string();
    self.on_commit = Some(commit);
    self.prev = OnPrev::Updated(Config::from_source(PrevSource::open_at(".", id.clone())?)?);

    for (proj_id, sized_pr) in &mut self.on_pr_sizes {
      if let Some(cur_project) = self.current.get_project(*proj_id) {
        let size = cur_project.size(&self.current.file.sizes, &kind)?;
        sized_pr.commits.push((id.clone(), summary.clone(), size, false));
      }
    }

    Ok(())
  }

  pub fn finish_commit(&mut self) -> Result<()> {
    Ok(())
  }

  pub fn consider_file(&mut self, path: &str) -> Result<()> {
    let commit = self.on_commit.as_ref().ok_or_else(|| versio_error!("Not on a commit"))?;
    let commit_id = commit.id();

    for prev_project in &self.prev.file().projects {
      if let Some(sized_pr) = self.on_pr_sizes.get_mut(&prev_project.id) {
        if prev_project.does_cover(path, &self.prev.source())? {
          let (_, _, _, applies) = sized_pr.commits.iter_mut().find(|(id, _, _, _)| id == commit_id).unwrap();
          *applies = true;
        }
      }
    }
    Ok(())
  }

  pub fn finish_file(&mut self) -> Result<()> {
    Ok(())
  }

  pub fn consider_deps(&mut self) -> Result<()> {
    // Use a modified Kahn's algorithm to traverse deps in order.
    let mut queue: VecDeque<(u32, Size)> = VecDeque::new();

    let mut dependents: HashMap<u32, HashSet<u32>> = HashMap::new();
    for project in &self.current.file.projects {
      for dep in &project.depends {
        dependents.entry(*dep).or_insert_with(HashSet::new).insert(project.id);
      }

      if project.depends.is_empty() {
        if let Some((size, _)) = self.incrs.get(&project.id) {
          queue.push_back((project.id, *size));
        } else {
          queue.push_back((project.id, Size::None))
        }
      }
    }

    while let Some((id, size)) = queue.pop_front() {
      let val = &mut self.incrs.entry(id).or_insert((Size::None, ChangeLog::empty())).0;
      *val = max(*val, size);

      let depds: Option<HashSet<u32>> = dependents.get(&id).cloned();
      if let Some(depds) = depds {
        for depd in depds {
          dependents.get_mut(&id).unwrap().remove(&depd);
          let val = &mut self.incrs.entry(depd).or_insert((Size::None, ChangeLog::empty())).0;
          *val = max(*val, size);

          if dependents.values().all(|ds| !ds.contains(&depd)) {
            queue.push_back((depd, *val));
          }
        }
      }
    }

    Ok(())
  }

  pub fn finish_plan(self) -> Result<Plan> {
    Ok(Plan { incrs: self.incrs, ineffective: self.ineffective })

    // let incrs = self
    //   .current
    //   .file
    //   .projects
    //   .iter()
    //   .map(|p| if let Some(size) = self.incrs.get(&p.id) {
    //     (p.id, *size, ChangeLog::empty())
    //   } else {
    //     (p.id, Size::None, ChangeLog::empty())
    //   })
    //   .collect();
  }
}

enum OnPrev<'s> {
  Initial(&'s Config<&'s PrevSource>),
  Updated(Config<PrevSource>)
}

impl<'s> OnPrev<'s> {
  pub fn source(&self) -> &PrevSource {
    match self {
      OnPrev::Initial(s) => &s.source,
      OnPrev::Updated(s) => &s.source
    }
  }

  pub fn file(&self) -> &ConfigFile {
    match self {
      OnPrev::Initial(s) => &s.file,
      OnPrev::Updated(s) => &s.file
    }
  }
}

#[derive(Deserialize, Debug)]
pub struct ConfigFile {
  projects: Vec<Project>,
  #[serde(deserialize_with = "deserialize_sizes", default)]
  sizes: HashMap<String, Size>
}

impl ConfigFile {
  pub fn load(source: &dyn Source) -> Result<ConfigFile> {
    match source.load(CONFIG_FILENAME.as_ref())? {
      Some(data) => ConfigFile::read(data.data()),
      None => Ok(ConfigFile::empty())
    }
  }

  pub fn empty() -> ConfigFile { ConfigFile { projects: Vec::new(), sizes: HashMap::new() } }

  pub fn read(data: &str) -> Result<ConfigFile> {
    let file: ConfigFile = serde_yaml::from_str(data)?;
    file.validate()?;
    Ok(file)
  }

  /// Check that IDs are unique, etc.
  fn validate(&self) -> Result<()> {
    let mut ids = HashSet::new();
    for p in &self.projects {
      if ids.contains(&p.id) {
        return versio_err!("Id {} is duplicated", p.id);
      }
      ids.insert(p.id);
    }

    // TODO: no circular deps

    Ok(())
  }
}

#[derive(Deserialize, Debug)]
pub struct Project {
  name: String,
  id: u32,
  #[serde(default)]
  covers: Vec<String>,
  #[serde(default)]
  depends: Vec<u32>,
  located: Location
}

impl Project {
  fn annotate(&self, source: &dyn Source) -> Result<AnnotatedMark> {
    Ok(AnnotatedMark::new(self.id, self.name.clone(), self.located.get_mark(source)?))
  }

  pub fn name(&self) -> &str { &self.name }
  pub fn id(&self) -> u32 { self.id }

  fn get_mark(&self, source: &dyn Source) -> Result<MarkedData> { self.located.get_mark(source) }

  fn size(&self, parent_sizes: &HashMap<String, Size>, kind: &str) -> Result<Size> {
    let kind = kind.trim();
    if kind.ends_with('!') {
      return Ok(Size::Major);
    }
    parent_sizes.get(kind).copied().map(Ok).unwrap_or_else(|| {
      parent_sizes.get("*").copied().map(Ok).unwrap_or_else(|| versio_err!("Unknown kind \"{}\".", kind))
    })
  }

  pub fn does_cover(&self, path: &str, _source: &dyn Source) -> Result<bool> {
    self.covers.iter().fold(Ok(false), |val, cov| {
      if val.is_err() || *val.as_ref().unwrap() {
        return val;
      }
      Ok(Pattern::new(cov)?.matches_with(path, match_opts()))
    })
  }

  fn check(&self, source: &dyn Source) -> Result<()> {
    self.located.get_mark(source)?;
    for cover in &self.covers {
      let cover = absolutize_pattern(cover, source.root_dir());
      if glob_with(&cover, match_opts())?.count() == 0 {
        return versio_err!("No files covered by \"{}\".", cover);
      }
    }
    Ok(())
  }

  fn show(&self, source: &dyn Source, name_width: usize, format: &ShowFormat) -> Result<()> {
    let mark = self.located.get_mark(source)?;
    if format.version_only {
      println!("{}", mark.value());
    } else if format.wide {
      println!("{:>4}. {:width$} : {}", self.id, self.name, mark.value(), width = name_width);
    } else {
      println!("{:width$} : {}", self.name, mark.value(), width = name_width);
    }
    Ok(())
  }

  fn set_value(&self, source: &dyn Source, val: &str) -> Result<()> {
    let mut mark = self.located.get_mark(source)?;
    mark.write_new_value(val)
  }
}

#[derive(Deserialize, Debug)]
struct Location {
  file: String,
  #[serde(flatten)]
  picker: Picker
}

impl Location {
  pub fn get_mark(&self, source: &dyn Source) -> Result<MarkedData> {
    let data = source.load(&self.file.as_ref())?.ok_or_else(|| versio_error!("No file at {}.", self.file))?;
    self.picker.get_mark(data).map_err(|e| versio_error!("Can't mark {}: {:?}", self.file, e))
  }
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum Picker {
  Json(JsonPicker),
  Yaml(YamlPicker),
  Toml(TomlPicker),
  Line(LinePicker),
  File(FilePicker)
}

impl Picker {
  pub fn _type(&self) -> &'static str {
    match self {
      Picker::Json(_) => "json",
      Picker::Yaml(_) => "yaml",
      Picker::Toml(_) => "toml",
      Picker::Line(_) => "line",
      Picker::File(_) => "file"
    }
  }

  pub fn get_mark(&self, data: NamedData) -> Result<MarkedData> {
    match self {
      Picker::Json(p) => p.scan(data),
      Picker::Yaml(p) => p.scan(data),
      Picker::Toml(p) => p.scan(data),
      Picker::Line(p) => p.scan(data),
      Picker::File(p) => p.scan(data)
    }
  }
}

#[derive(Deserialize, Debug)]
struct JsonPicker {
  #[serde(deserialize_with = "deserialize_parts")]
  json: Vec<Part>
}

impl JsonPicker {
  pub fn scan(&self, data: NamedData) -> Result<MarkedData> { JsonScanner::new(self.json.clone()).scan(data) }
}

#[derive(Deserialize, Debug)]
struct YamlPicker {
  #[serde(deserialize_with = "deserialize_parts")]
  yaml: Vec<Part>
}

impl YamlPicker {
  pub fn scan(&self, data: NamedData) -> Result<MarkedData> { YamlScanner::new(self.yaml.clone()).scan(data) }
}

#[derive(Deserialize, Debug)]
struct TomlPicker {
  #[serde(deserialize_with = "deserialize_parts")]
  toml: Vec<Part>
}

impl TomlPicker {
  pub fn scan(&self, data: NamedData) -> Result<MarkedData> { TomlScanner::new(self.toml.clone()).scan(data) }
}

#[derive(Deserialize, Debug)]
struct LinePicker {
  pattern: String
}

impl LinePicker {
  pub fn scan(&self, data: NamedData) -> Result<MarkedData> { find_reg_data(data, &self.pattern) }
}

fn find_reg_data(data: NamedData, pattern: &str) -> Result<MarkedData> {
  let pattern = Regex::new(pattern)?;
  let found = pattern.captures(data.data()).ok_or_else(|| versio_error!("No match for {}", pattern))?;
  let item = found.get(1).ok_or_else(|| versio_error!("No capture group in {}.", pattern))?;
  let value = item.as_str().to_string();
  let index = item.start();
  Ok(data.mark(Mark::make(value, index)?))
}

#[derive(Deserialize, Debug)]
struct FilePicker {}

impl FilePicker {
  pub fn scan(&self, data: NamedData) -> Result<MarkedData> {
    let value = data.data().trim_end().to_string();
    Ok(data.mark(Mark::make(value, 0)?))
  }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Size {
  Fail,
  Major,
  Minor,
  Patch,
  None
}

impl Size {
  fn is_size(v: &str) -> bool { Size::from_str(v).is_ok() }

  fn from_str(v: &str) -> Result<Size> {
    match v {
      "major" => Ok(Size::Major),
      "minor" => Ok(Size::Minor),
      "patch" => Ok(Size::Patch),
      "none" => Ok(Size::None),
      "fail" => Ok(Size::Fail),
      other => versio_err!("Unknown size: {}", other)
    }
  }

  fn parts(v: &str) -> Result<[u32; 3]> {
    let parts: Vec<_> = v.split('.').map(|p| p.parse()).collect::<std::result::Result<_, _>>()?;
    if parts.len() != 3 {
      return versio_err!("Not a 3-part version: {}", v);
    }
    Ok([parts[0], parts[1], parts[2]])
  }

  pub fn less_than(v1: &str, v2: &str) -> Result<bool> {
    let p1 = Size::parts(v1)?;
    let p2 = Size::parts(v2)?;

    Ok(p1[0] < p2[0] || (p1[0] == p2[0] && (p1[1] < p2[1] || (p1[1] == p2[1] && p1[2] < p2[2]))))
  }

  pub fn apply(self, v: &str) -> Result<String> {
    let parts = Size::parts(v)?;

    let newv = match self {
      Size::Major => format!("{}.{}.{}", parts[0] + 1, 0, 0),
      Size::Minor => format!("{}.{}.{}", parts[0], parts[1] + 1, 0),
      Size::Patch => format!("{}.{}.{}", parts[0], parts[1], parts[2] + 1),
      Size::None => format!("{}.{}.{}", parts[0], parts[1], parts[2]),
      Size::Fail => return versio_err!("'fail' size encountered.")
    };

    Ok(newv)
  }
}

impl fmt::Display for Size {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    match self {
      Size::Major => write!(f, "major"),
      Size::Minor => write!(f, "minor"),
      Size::Patch => write!(f, "patch"),
      Size::None => write!(f, "none"),
      Size::Fail => write!(f, "fail")
    }
  }
}

impl PartialOrd for Size {
  fn partial_cmp(&self, other: &Size) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl Ord for Size {
  fn cmp(&self, other: &Size) -> Ordering {
    match self {
      Size::Fail => match other {
        Size::Fail => Ordering::Equal,
        _ => Ordering::Greater
      },
      Size::Major => match other {
        Size::Fail => Ordering::Less,
        Size::Major => Ordering::Equal,
        _ => Ordering::Greater
      },
      Size::Minor => match other {
        Size::Major | Size::Fail => Ordering::Less,
        Size::Minor => Ordering::Equal,
        _ => Ordering::Greater
      },
      Size::Patch => match other {
        Size::None => Ordering::Greater,
        Size::Patch => Ordering::Equal,
        _ => Ordering::Less
      },
      Size::None => match other {
        Size::None => Ordering::Equal,
        _ => Ordering::Less
      }
    }
  }
}

fn deserialize_sizes<'de, D: Deserializer<'de>>(desr: D) -> std::result::Result<HashMap<String, Size>, D::Error> {
  struct MapVisitor;

  impl<'de> Visitor<'de> for MapVisitor {
    type Value = HashMap<String, Size>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result { formatter.write_str("a list of sizes") }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
      M: MapAccess<'de>
    {
      let mut result = HashMap::new();
      let mut using_angular = false;

      while let Some(val) = map.next_key::<String>()? {
        match val.as_str() {
          val if Size::is_size(val) => {
            let size = Size::from_str(val).unwrap();
            let keys: Vec<String> = map.next_value()?;
            for key in keys {
              if result.contains_key(&key) {
                return Err(de::Error::custom(format!("Duplicated kind \"{}\".", key)));
              }
              result.insert(key, size);
            }
          }
          "use_angular" => {
            using_angular = map.next_value()?;
          }
          _ => return Err(de::Error::custom(format!("Unrecognized sizes key \"{}\".", val)))
        }
      }

      // Based on the angular standard:
      // https://github.com/angular/angular.js/blob/master/DEVELOPERS.md#-git-commit-guidelines
      if using_angular {
        insert_if_missing(&mut result, "feat", Size::Minor);
        insert_if_missing(&mut result, "fix", Size::Patch);
        insert_if_missing(&mut result, "docs", Size::None);
        insert_if_missing(&mut result, "style", Size::None);
        insert_if_missing(&mut result, "refactor", Size::None);
        insert_if_missing(&mut result, "perf", Size::None);
        insert_if_missing(&mut result, "test", Size::None);
        insert_if_missing(&mut result, "chore", Size::None);
        insert_if_missing(&mut result, "build", Size::None);
      }

      Ok(result)
    }
  }

  desr.deserialize_map(MapVisitor)
}

fn insert_if_missing(result: &mut HashMap<String, Size>, key: &str, val: Size) {
  if !result.contains_key(key) {
    result.insert(key.to_string(), val);
  }
}

fn match_opts() -> MatchOptions { MatchOptions { require_literal_separator: true, ..Default::default() } }

fn absolutize_pattern<'a>(cover: &'a str, root_dir: &Path) -> Cow<'a, str> {
  let cover = Path::new(cover);
  if !cover.has_root() {
    Cow::Owned(root_dir.join(cover).to_string_lossy().into_owned())
  } else {
    Cow::Borrowed(cover.to_str().unwrap())
  }
}

#[cfg(test)]
mod test {
  use super::{find_reg_data, ConfigFile, Size};
  use crate::NamedData;

  #[test]
  fn test_scan() {
    let data = r#"
projects:
  - name: everything
    id: 1
    covers: ["**"]
    located:
      file: "toplevel.json"
      json: "version"

  - name: project1
    id: 2
    covers: ["project1/**"]
    located:
      file: "project1/Cargo.toml"
      toml: "version"

  - name: "combined a and b"
    id: 3
    covers: ["nested/project_a/**", "nested/project_b/**"]
    located:
      file: "nested/version.txt"
      pattern: "v([0-9]+\\.[0-9]+\\.[0-9]+) .*"

  - name: "build image"
    id: 4
    depends: [2, 3]
    located:
      file: "build/VERSION""#;

    let config = ConfigFile::read(data).unwrap();

    assert_eq!(config.projects[0].id, 1);
    assert_eq!("line", config.projects[2].located.picker._type());
  }

  #[test]
  fn test_validate() {
    let config = r#"
projects:
  - name: p1
    id: 1
    covers: ["**"]
    located: { file: f1 }

  - name: project1
    id: 1
    covers: ["**"]
    located: { file: f2 }
    "#;

    assert!(ConfigFile::read(config).is_err());
  }

  #[test]
  fn test_find_reg() {
    let data = r#"
This is text.
Current rev is "v1.2.3" because it is."#;

    let marked_data = find_reg_data(NamedData::new(None, data.to_string()), "v(\\d+\\.\\d+\\.\\d+)").unwrap();
    assert_eq!("1.2.3", marked_data.value());
    assert_eq!(32, marked_data.start());
  }

  #[test]
  fn test_sizes() {
    let config = r#"
projects: []
sizes:
  major: [ break ]
  minor: [ feat ]
  patch: [ fix, "-" ]
  none: [ none ]
"#;

    let config = ConfigFile::read(config).unwrap();
    assert_eq!(&Size::Minor, config.sizes.get("feat").unwrap());
    assert_eq!(&Size::Major, config.sizes.get("break").unwrap());
    assert_eq!(&Size::Patch, config.sizes.get("fix").unwrap());
    assert_eq!(&Size::Patch, config.sizes.get("-").unwrap());
    assert_eq!(&Size::None, config.sizes.get("none").unwrap());
  }

  #[test]
  fn test_sizes_dup() {
    let config = r#"
projects: []
sizes:
  major: [ break, feat ]
  minor: [ feat ]
  patch: [ fix, "-" ]
  none: [ none ]
"#;

    assert!(ConfigFile::read(config).is_err());
  }
}
