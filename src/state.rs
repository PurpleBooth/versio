//! The mechanisms used to read and write state, both current and historical.

use crate::config::ProjectId;
use crate::errors::Result;
use crate::git::{FromTagBuf, Repo, Slice};
use crate::mark::{NamedData, Picker};
use log::{trace, warn};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub trait StateRead: FilesRead {
  fn latest_tag(&self, proj: &ProjectId) -> Option<&String>;
}

impl<S: StateRead> StateRead for &S {
  fn latest_tag(&self, proj: &ProjectId) -> Option<&String> { <S as StateRead>::latest_tag(*self, proj) }
}

pub trait FilesRead {
  fn has_file(&self, path: &Path) -> Result<bool>;
  fn read_file(&self, path: &Path) -> Result<String>;
  fn subdirs(&self, root: Option<&String>, regex: &str) -> Result<Vec<String>>;
}

impl<F: FilesRead> FilesRead for &F {
  fn has_file(&self, path: &Path) -> Result<bool> { <F as FilesRead>::has_file(*self, path) }
  fn read_file(&self, path: &Path) -> Result<String> { <F as FilesRead>::read_file(*self, path) }
  fn subdirs(&self, root: Option<&String>, regex: &str) -> Result<Vec<String>> {
    <F as FilesRead>::subdirs(*self, root, regex)
  }
}

pub struct CurrentState {
  files: CurrentFiles,
  tags: OldTags
}

impl FilesRead for CurrentState {
  fn has_file(&self, path: &Path) -> Result<bool> { self.files.has_file(path) }
  fn read_file(&self, path: &Path) -> Result<String> { self.files.read_file(path) }
  fn subdirs(&self, root: Option<&String>, regex: &str) -> Result<Vec<String>> { self.files.subdirs(root, regex) }
}

impl StateRead for CurrentState {
  fn latest_tag(&self, proj: &ProjectId) -> Option<&String> { self.tags.latest(proj) }
}

impl CurrentState {
  pub fn new(root: PathBuf, tags: OldTags) -> CurrentState { CurrentState { files: CurrentFiles::new(root), tags } }

  pub fn slice<'r>(&self, spec: FromTagBuf, repo: &'r Repo) -> Result<PrevState<'r>> {
    let old_tags = match repo.revparse_oid(spec.as_from_tag()) {
      Ok(commit_oid) => self.tags.slice_earlier(&commit_oid)?,
      Err(e) => {
        if spec.is_else_none() {
          OldTags::empty()
        } else {
          return Err(e);
        }
      }
    };

    Ok(PrevState::new(repo.slice(spec), old_tags))
  }
}

pub struct CurrentFiles {
  root: PathBuf
}

impl FilesRead for CurrentFiles {
  fn has_file(&self, path: &Path) -> Result<bool> { Ok(self.root.join(path).exists()) }
  fn read_file(&self, path: &Path) -> Result<String> { Ok(std::fs::read_to_string(&self.root.join(path))?) }

  fn subdirs(&self, root: Option<&String>, regex: &str) -> Result<Vec<String>> {
    let filter = Regex::new(regex)?;
    let root = root.map(|s| s.as_str()).unwrap_or(".");
    Path::new(root)
      .read_dir()?
      .filter_map(|e| e.map(|e| e.file_name().into_string().ok()).transpose())
      .filter(|n| n.as_ref().map(|n| filter.is_match(&n)).unwrap_or(true))
      .map(|r| r.map_err(|e| e.into()))
      .collect()
  }
}

impl CurrentFiles {
  pub fn new(root: PathBuf) -> CurrentFiles { CurrentFiles { root } }
}

pub struct PrevState<'r> {
  files: PrevFiles<'r>,
  tags: OldTags
}

impl<'r> FilesRead for PrevState<'r> {
  fn has_file(&self, path: &Path) -> Result<bool> { self.files.has_file(path) }
  fn read_file(&self, path: &Path) -> Result<String> { self.files.read_file(path) }
  fn subdirs(&self, root: Option<&String>, regex: &str) -> Result<Vec<String>> { self.files.subdirs(root, regex) }
}

impl<'r> StateRead for PrevState<'r> {
  fn latest_tag(&self, proj: &ProjectId) -> Option<&String> { self.tags.latest(proj) }
}

impl<'r> PrevState<'r> {
  fn new(slice: Slice<'r>, tags: OldTags) -> PrevState { PrevState { files: PrevFiles::new(slice), tags } }
}

pub struct PrevFiles<'r> {
  slice: Slice<'r>
}

impl<'r> FilesRead for PrevFiles<'r> {
  fn has_file(&self, path: &Path) -> Result<bool> { self.slice.has_blob(&path.to_string_lossy().to_string()) }
  fn read_file(&self, path: &Path) -> Result<String> { read_from_slice(&self.slice, path) }

  fn subdirs(&self, root: Option<&String>, regex: &str) -> Result<Vec<String>> { self.slice.subdirs(root, regex) }
}

impl<'r> PrevFiles<'r> {
  pub fn from_slice(slice: Slice<'r>) -> Result<PrevFiles> { Ok(PrevFiles::new(slice)) }

  pub fn new(slice: Slice<'r>) -> PrevFiles { PrevFiles { slice } }
  pub fn slice_to(&self, spec: FromTagBuf) -> Result<PrevFiles<'r>> { PrevFiles::from_slice(self.slice.slice(spec)) }
}

#[derive(Debug)]
pub struct OldTags {
  by_proj: HashMap<ProjectId, Vec<String>>,
  not_after: HashMap<ProjectId, HashMap<String, usize>>
}

impl OldTags {
  pub fn new(
    by_proj: HashMap<ProjectId, Vec<String>>, not_after: HashMap<ProjectId, HashMap<String, usize>>
  ) -> OldTags {
    OldTags { by_proj, not_after }
  }

  pub fn empty() -> OldTags { OldTags { by_proj: HashMap::new(), not_after: HashMap::new() } }

  fn latest(&self, proj: &ProjectId) -> Option<&String> { self.by_proj.get(proj).and_then(|p| p.first()) }

  /// Construct a tags index for an earlier commit; a `latest` call on the returned index will match the
  /// `not_after(new_oid)` on this index.
  pub fn slice_earlier(&self, new_oid: &str) -> Result<OldTags> {
    let mut by_proj = HashMap::new();
    let mut not_after = HashMap::new();

    for (proj_id, afts) in &self.not_after {
      if let Some(&ind) = afts.get(new_oid) {
        let list = self.by_proj.get(proj_id).ok_or_else(|| bad!("Illegal proj {} oid for {}", proj_id, new_oid))?;
        let list = list[ind ..].to_vec();
        by_proj.insert(proj_id.clone(), list);

        let new_afts =
          afts.iter().filter_map(|(oid, i)| if i >= &ind { Some((oid.clone(), i - ind)) } else { None }).collect();
        not_after.insert(proj_id.clone(), new_afts);
      }
    }

    Ok(OldTags::new(by_proj, not_after))
  }
}

pub struct StateWrite {
  writes: Vec<FileWrite>,
  proj_writes: HashSet<ProjectId>,
  tag_head: Vec<String>,
  tag_commit: HashMap<String, String>,
  tag_head_or_last: Vec<(String, ProjectId)>
}

impl Default for StateWrite {
  fn default() -> StateWrite { StateWrite::new() }
}

impl StateWrite {
  pub fn new() -> StateWrite {
    StateWrite {
      writes: Vec::new(),
      tag_head: Vec::new(),
      tag_commit: HashMap::new(),
      tag_head_or_last: Vec::new(),
      proj_writes: HashSet::new()
    }
  }

  pub fn write_file<C: ToString>(&mut self, file: PathBuf, content: C, proj_id: &ProjectId) -> Result<()> {
    self.writes.push(FileWrite::Write { path: file, val: content.to_string() });
    self.proj_writes.insert(proj_id.clone());
    Ok(())
  }

  pub fn update_mark<C: ToString>(&mut self, pick: PickPath, content: C, proj_id: &ProjectId) -> Result<()> {
    self.writes.push(FileWrite::Update { pick, val: content.to_string() });
    self.proj_writes.insert(proj_id.clone());
    Ok(())
  }

  pub fn tag_head_or_last<T: ToString>(&mut self, tag: T, proj: &ProjectId) -> Result<()> {
    let tag = tag.to_string();
    trace!("head_or_last on {} tagged with {}.", proj, tag);
    self.tag_head_or_last.push((tag, proj.clone()));
    Ok(())
  }

  pub fn commit(&mut self, repo: &Repo, prev_tag: &str, last_commits: &HashMap<ProjectId, String>) -> Result<()> {
    for write in &self.writes {
      write.write()?;
    }
    let did_write = !self.writes.is_empty();
    self.writes.clear();

    if did_write {
      repo.commit()?;
    }

    for tag in &self.tag_head {
      repo.update_tag_head(tag)?;
    }
    self.tag_head.clear();

    for (tag, proj_id) in &self.tag_head_or_last {
      if self.proj_writes.contains(&proj_id) {
        repo.update_tag_head(tag)?;
      } else if let Some(oid) = last_commits.get(proj_id) {
        repo.update_tag(tag, oid)?;
      } else {
        warn!("Latest commit for project {} unknown: tagging head.", proj_id);
        repo.update_tag_head(tag)?;
      }
    }
    self.tag_head_or_last.clear();
    self.proj_writes.clear();

    for (tag, oid) in &self.tag_commit {
      repo.update_tag(tag, oid)?;
    }
    self.tag_commit.clear();

    repo.update_tag_head(prev_tag)?;

    Ok(())
  }
}

enum FileWrite {
  Write { path: PathBuf, val: String },
  Update { pick: PickPath, val: String }
}

impl FileWrite {
  pub fn write(&self) -> Result<()> {
    match self {
      FileWrite::Write { path, val } => Ok(std::fs::write(path, &val)?),
      // FileWrite::Append { path, val } => {
      //   let mut file = OpenOptions::new().append(true).open(path)?;
      //   Ok(file.write_all(val.as_bytes())?)
      // }
      FileWrite::Update { pick, val } => pick.write_value(val)
    }
  }
}

pub struct PickPath {
  file: PathBuf,
  picker: Picker
}

impl PickPath {
  pub fn new(file: PathBuf, picker: Picker) -> PickPath { PickPath { file, picker } }

  pub fn write_value(&self, val: &str) -> Result<()> {
    let data = std::fs::read_to_string(&self.file)?;
    let data = NamedData::new(self.file.clone(), data);

    let mut mark = self.picker.scan(data)?;
    mark.write_new_value(val)?;
    Ok(())
  }
}

pub fn read_from_slice<P: AsRef<Path>>(slice: &Slice, path: P) -> Result<String> {
  let blob = slice.blob(&path.as_ref().to_string_lossy().to_string())?;
  let cont: &str = std::str::from_utf8(blob.content())?;
  Ok(cont.to_string())
}
