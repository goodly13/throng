//! Projects (Principle I): the root context of the application.
//!
//! A project has a friendly name, a dominant colour and a root folder that is **exclusively** bound
//! to it — no two projects may share a root, and no root may sit inside another's.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::ProjectId;
use crate::paths::PathRules;

/// The longest friendly name accepted.
pub const MAX_NAME_LENGTH: usize = 120;

/// A project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub colour: Colour,
    pub root: PathBuf,
    /// Root-relative, `/`-separated paths hidden from the file tree for this project only.
    #[serde(default)]
    pub hidden_paths: Vec<String>,
    /// Unix milliseconds.
    pub created_at: i64,
    pub updated_at: i64,
}

/// The fields a user edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectInput {
    pub name: String,
    pub colour: String,
    pub root: PathBuf,
}

/// Why a project input was refused. Each names the field it is about, so the dialog can show the
/// message beside that field and nowhere else.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ProjectError {
    #[error("Give the project a name.")]
    EmptyName,
    #[error("A project name can be at most {MAX_NAME_LENGTH} characters.")]
    NameTooLong,
    #[error("\"{0}\" is not a colour. Use a hex value such as #6aa3ff.")]
    InvalidColour(String),
    #[error("Choose the project's root folder.")]
    EmptyRoot,
    #[error("The root folder must be a full path.")]
    RootNotAbsolute,
    #[error("This folder overlaps the root of \"{other_name}\" ({}). Each folder belongs to one project.", other_root.display())]
    FolderConflict { other_name: String, other_root: PathBuf },
    #[error("No project with that id exists.")]
    NotFound,
}

impl ProjectError {
    /// The input field the error is about.
    #[must_use]
    pub fn field(&self) -> ProjectField {
        match self {
            Self::EmptyName | Self::NameTooLong => ProjectField::Name,
            Self::InvalidColour(_) => ProjectField::Colour,
            Self::EmptyRoot | Self::RootNotAbsolute | Self::FolderConflict { .. } => ProjectField::Root,
            Self::NotFound => ProjectField::Name,
        }
    }
}

/// An editable project field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectField {
    Name,
    Colour,
    Root,
}

/// An sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Colour {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Colour {
    /// Parse `#rgb` or `#rrggbb` (case-insensitive).
    pub fn parse(text: &str) -> Result<Self, ProjectError> {
        let bad = || ProjectError::InvalidColour(text.to_owned());
        let hex = text.trim().strip_prefix('#').ok_or_else(bad)?;
        let digit =
            |c: u8| -> Result<u8, ProjectError> { (c as char).to_digit(16).map(|d| d as u8).ok_or_else(bad) };
        let bytes = hex.as_bytes();
        match bytes.len() {
            3 => {
                let (r, g, b) = (digit(bytes[0])?, digit(bytes[1])?, digit(bytes[2])?);
                Ok(Self { r: r * 17, g: g * 17, b: b * 17 })
            }
            6 => Ok(Self {
                r: digit(bytes[0])? * 16 + digit(bytes[1])?,
                g: digit(bytes[2])? * 16 + digit(bytes[3])?,
                b: digit(bytes[4])? * 16 + digit(bytes[5])?,
            }),
            _ => Err(bad()),
        }
    }

    /// `#rrggbb`, lower case.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Relative luminance (WCAG), for choosing readable text over this colour.
    #[must_use]
    pub fn luminance(self) -> f32 {
        fn channel(c: u8) -> f32 {
            let c = f32::from(c) / 255.0;
            if c <= 0.039_28 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        }
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }
}

impl TryFrom<String> for Colour {
    type Error = ProjectError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Colour> for String {
    fn from(value: Colour) -> Self {
        value.to_hex()
    }
}

/// The colours offered for a new project, in order.
pub const PALETTE: [&str; 10] = [
    "#6aa3ff", "#4cc38a", "#f5a524", "#e5484d", "#8e4ec6", "#12a594", "#f76b15", "#d6409f", "#978365",
    "#0090ff",
];

/// A validated, normalised input: trimmed name, parsed colour, trimmed root.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidInput {
    pub name: String,
    pub colour: Colour,
    pub root: PathBuf,
}

/// Validate an input's fields on their own (not against other projects).
pub fn validate_input(input: &ProjectInput) -> Result<ValidInput, ProjectError> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ProjectError::EmptyName);
    }
    if name.chars().count() > MAX_NAME_LENGTH {
        return Err(ProjectError::NameTooLong);
    }
    let colour = Colour::parse(&input.colour)?;
    let root_text = input.root.to_string_lossy();
    let root_trimmed = root_text.trim();
    if root_trimmed.is_empty() {
        return Err(ProjectError::EmptyRoot);
    }
    let root = PathBuf::from(root_trimmed);
    if !root.is_absolute() {
        return Err(ProjectError::RootNotAbsolute);
    }
    Ok(ValidInput { name: name.to_owned(), colour, root })
}

/// Refuse `candidate` if it overlaps any other project's root. `self_id` is the project
/// being edited, which may keep its own root.
pub fn assert_folder_exclusive<'a>(
    rules: &PathRules,
    candidate: &Path,
    existing: impl IntoIterator<Item = &'a Project>,
    self_id: Option<ProjectId>,
) -> Result<(), ProjectError> {
    for project in existing {
        if Some(project.id) == self_id {
            continue;
        }
        if rules.overlaps(candidate, &project.root) {
            return Err(ProjectError::FolderConflict {
                other_name: project.name.clone(),
                other_root: project.root.clone(),
            });
        }
    }
    Ok(())
}

/// The user's projects in display order, with at most one active (Principle I).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectBook {
    projects: Vec<Project>,
    active: Option<ProjectId>,
}

impl ProjectBook {
    /// Build from persisted state. An `active` id that names no project is dropped.
    #[must_use]
    pub fn new(projects: Vec<Project>, active: Option<ProjectId>) -> Self {
        let active = active.filter(|id| projects.iter().any(|p| p.id == *id));
        Self { projects, active }
    }

    #[must_use]
    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    #[must_use]
    pub fn active_id(&self) -> Option<ProjectId> {
        self.active
    }

    #[must_use]
    pub fn active(&self) -> Option<&Project> {
        self.active.and_then(|id| self.get(id))
    }

    #[must_use]
    pub fn get(&self, id: ProjectId) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// Create a project. The first project becomes active.
    pub fn create(
        &mut self,
        rules: &PathRules,
        input: &ProjectInput,
        now: i64,
    ) -> Result<&Project, ProjectError> {
        let valid = validate_input(input)?;
        assert_folder_exclusive(rules, &valid.root, &self.projects, None)?;
        let project = Project {
            id: ProjectId::new(),
            name: valid.name,
            colour: valid.colour,
            root: valid.root,
            hidden_paths: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        if self.active.is_none() {
            self.active = Some(project.id);
        }
        self.projects.push(project);
        Ok(self.projects.last().expect("just pushed"))
    }

    /// Edit a project's name, colour and root.
    pub fn update(
        &mut self,
        rules: &PathRules,
        id: ProjectId,
        input: &ProjectInput,
        now: i64,
    ) -> Result<&Project, ProjectError> {
        let valid = validate_input(input)?;
        assert_folder_exclusive(rules, &valid.root, &self.projects, Some(id))?;
        let project = self.projects.iter_mut().find(|p| p.id == id).ok_or(ProjectError::NotFound)?;
        project.name = valid.name;
        project.colour = valid.colour;
        project.root = valid.root;
        project.updated_at = now;
        Ok(project)
    }

    /// Replace a project's hidden paths (deduplicated, empties dropped).
    pub fn set_hidden(&mut self, id: ProjectId, hidden: Vec<String>, now: i64) -> Result<(), ProjectError> {
        let project = self.projects.iter_mut().find(|p| p.id == id).ok_or(ProjectError::NotFound)?;
        let mut seen = Vec::new();
        for path in hidden {
            if !path.is_empty() && !seen.contains(&path) {
                seen.push(path);
            }
        }
        project.hidden_paths = seen;
        project.updated_at = now;
        Ok(())
    }

    /// Make `id` the active project.
    pub fn set_active(&mut self, id: ProjectId) -> Result<(), ProjectError> {
        if self.get(id).is_none() {
            return Err(ProjectError::NotFound);
        }
        self.active = Some(id);
        Ok(())
    }

    /// Delete a project. If it was active, the first remaining project becomes active. Returns the
    /// removed project.
    pub fn delete(&mut self, id: ProjectId) -> Result<Project, ProjectError> {
        let index = self.projects.iter().position(|p| p.id == id).ok_or(ProjectError::NotFound)?;
        let removed = self.projects.remove(index);
        if self.active == Some(id) {
            self.active = self.projects.first().map(|p| p.id);
        }
        Ok(removed)
    }

    /// Move a project to `to` in display order (clamped).
    pub fn reorder(&mut self, id: ProjectId, to: usize) -> Result<(), ProjectError> {
        let from = self.projects.iter().position(|p| p.id == id).ok_or(ProjectError::NotFound)?;
        let project = self.projects.remove(from);
        let to = to.min(self.projects.len());
        self.projects.insert(to, project);
        Ok(())
    }

    /// The project whose root contains `path`, if any.
    #[must_use]
    pub fn owner_of(&self, rules: &PathRules, path: &Path) -> Option<&Project> {
        self.projects.iter().find(|p| rules.is_within(&p.root, path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixtures are written as Unix paths. On Windows a path is only absolute with a drive, so
    /// one is added there; the rules under test are passed explicitly either way.
    fn host(path: &str) -> String {
        let rest = path.trim_start();
        if cfg!(windows) && rest.starts_with('/') {
            format!("{}C:{rest}", &path[..path.len() - rest.len()])
        } else {
            path.to_owned()
        }
    }

    fn input(name: &str, root: &str) -> ProjectInput {
        ProjectInput { name: name.into(), colour: "#6aa3ff".into(), root: host(root).into() }
    }

    #[test]
    fn colours_parse_and_print() {
        assert_eq!(Colour::parse("#fff").unwrap().to_hex(), "#ffffff");
        assert_eq!(Colour::parse("#6AA3FF").unwrap().to_hex(), "#6aa3ff");
        assert!(Colour::parse("6aa3ff").is_err());
        assert!(Colour::parse("#6aa3f").is_err());
        assert!(Colour::parse("#ggg").is_err());
    }

    #[test]
    fn inputs_are_trimmed_and_validated() {
        let v = validate_input(&input("  Web  ", " /work/web ")).unwrap();
        assert_eq!(v.name, "Web");
        assert_eq!(v.root, PathBuf::from(host("/work/web")));
        assert_eq!(validate_input(&input("  ", "/w")), Err(ProjectError::EmptyName));
        assert_eq!(validate_input(&input("x", "")), Err(ProjectError::EmptyRoot));
        assert_eq!(validate_input(&input("x", "relative/dir")), Err(ProjectError::RootNotAbsolute));
        let long = "x".repeat(MAX_NAME_LENGTH + 1);
        assert_eq!(validate_input(&input(&long, "/w")), Err(ProjectError::NameTooLong));
    }

    #[test]
    fn first_project_becomes_active_and_roots_are_exclusive() {
        let rules = PathRules::LINUX;
        let mut book = ProjectBook::default();
        let a = book.create(&rules, &input("A", "/work/a"), 1).unwrap().id;
        assert_eq!(book.active_id(), Some(a));
        let b = book.create(&rules, &input("B", "/work/b"), 2).unwrap().id;
        assert_eq!(book.active_id(), Some(a));

        let nested = book.create(&rules, &input("C", "/work/a/sub"), 3).unwrap_err();
        assert!(matches!(nested, ProjectError::FolderConflict { ref other_name, .. } if other_name == "A"));
        let parent = book.create(&rules, &input("C", "/work"), 3).unwrap_err();
        assert!(matches!(parent, ProjectError::FolderConflict { .. }));
        // Linux is case-sensitive: /work/A is a different folder.
        book.create(&rules, &input("Caps", "/work/A"), 3).unwrap();

        // Editing may keep its own root but not take another's.
        book.update(&rules, b, &input("B2", "/work/b"), 4).unwrap();
        assert!(book.update(&rules, b, &input("B2", "/work/a"), 4).is_err());
    }

    #[test]
    fn case_insensitive_platforms_catch_respellings() {
        let rules = PathRules::MACOS;
        let mut book = ProjectBook::default();
        book.create(&rules, &input("A", "/Users/me/Proj"), 1).unwrap();
        assert!(book.create(&rules, &input("B", "/users/me/proj"), 1).is_err());
    }

    #[test]
    fn deleting_the_active_project_promotes_the_first_remaining() {
        let rules = PathRules::LINUX;
        let mut book = ProjectBook::default();
        let a = book.create(&rules, &input("A", "/a"), 1).unwrap().id;
        let b = book.create(&rules, &input("B", "/b"), 1).unwrap().id;
        book.set_active(a).unwrap();
        book.delete(a).unwrap();
        assert_eq!(book.active_id(), Some(b));
        book.delete(b).unwrap();
        assert_eq!(book.active_id(), None);
    }

    #[test]
    fn reorder_and_owner_lookup() {
        let rules = PathRules::LINUX;
        let mut book = ProjectBook::default();
        let a = book.create(&rules, &input("A", "/a"), 1).unwrap().id;
        let b = book.create(&rules, &input("B", "/b"), 1).unwrap().id;
        book.reorder(b, 0).unwrap();
        assert_eq!(book.projects()[0].id, b);
        assert_eq!(book.owner_of(&rules, Path::new(&host("/a/x/y"))).map(|p| p.id), Some(a));
        assert!(book.owner_of(&rules, Path::new(&host("/c"))).is_none());
    }

    #[test]
    fn hidden_paths_are_deduplicated() {
        let rules = PathRules::LINUX;
        let mut book = ProjectBook::default();
        let a = book.create(&rules, &input("A", "/a"), 1).unwrap().id;
        book.set_hidden(a, vec!["x".into(), "".into(), "x".into(), "y".into()], 2).unwrap();
        assert_eq!(book.get(a).unwrap().hidden_paths, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn stale_active_id_is_dropped() {
        let book = ProjectBook::new(Vec::new(), Some(ProjectId::new()));
        assert_eq!(book.active_id(), None);
    }
}
