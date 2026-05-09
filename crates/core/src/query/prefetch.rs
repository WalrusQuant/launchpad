use std::path::{Path, PathBuf};

use lpa_protocol::{RequestContent, RequestMessage, UserInput};

use crate::Role;

const INSTRUCTION_FILE_NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

fn default_root_markers() -> Vec<String> {
    vec![".git".into()]
}

fn find_project_root(start: &Path, markers: &[String]) -> Option<PathBuf> {
    let mut current = start;
    loop {
        for marker in markers {
            if current.join(marker).exists() {
                return Some(current.to_path_buf());
            }
        }
        current = current.parent()?;
    }
}

fn walk_instruction_files(cwd: &Path, markers: &[String]) -> Vec<PathBuf> {
    let markers = if markers.is_empty() {
        default_root_markers()
    } else {
        markers.to_vec()
    };

    let root = match find_project_root(cwd, &markers) {
        Some(r) => r,
        None => return Vec::new(),
    };

    let mut ancestors = Vec::new();
    let mut current = Some(cwd);
    while let Some(dir) = current {
        if dir == root || dir.starts_with(&root) {
            ancestors.push(dir.to_path_buf());
        }
        if dir == root {
            break;
        }
        current = dir.parent();
    }

    let mut files = Vec::new();
    for dir in ancestors.iter().rev() {
        for file_name in INSTRUCTION_FILE_NAMES {
            let path = dir.join(file_name);
            if path.is_file() {
                let canonical = match path.canonicalize() {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if !files.contains(&canonical) {
                    files.push(canonical);
                }
            }
        }
    }

    files
}

pub(super) fn load_prompt_md(cwd: &Path) -> Option<String> {
    load_prompt_md_with_markers(cwd, &[])
}

pub(super) fn load_prompt_md_with_markers(cwd: &Path, markers: &[String]) -> Option<String> {
    let files = walk_instruction_files(cwd, markers);
    if files.is_empty() {
        return load_prompt_md_single(cwd);
    }

    let mut sections = Vec::new();
    for path in &files {
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let parent = path.parent().unwrap_or(cwd);
        if let Ok(content) = std::fs::read_to_string(path) {
            let content = content.trim().to_string();
            if !content.is_empty() {
                sections.push(format!(
                    "# {} instructions for {}\n\n <INSTRUCTIONS>\n{}\n</INSTRUCTIONS>",
                    file_name,
                    parent.display(),
                    content,
                ));
            }
        }
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

fn load_prompt_md_single(cwd: &Path) -> Option<String> {
    let mut sections = Vec::new();
    for file_name in INSTRUCTION_FILE_NAMES {
        let path = cwd.join(file_name);
        if let Ok(content) = std::fs::read_to_string(path) {
            let content = content.trim().to_string();
            if !content.is_empty() {
                sections.push(format!(
                    "# {} instructions for {}\n\n <INSTRUCTIONS>\n{}\n</INSTRUCTIONS>",
                    file_name,
                    cwd.display(),
                    content,
                ));
            }
        }
    }
    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

pub(super) fn build_system_prompt(base_instructions: &str) -> String {
    let mut sections = Vec::new();
    if !base_instructions.is_empty() {
        sections.push(base_instructions.to_string());
    }
    sections.join("\n\n")
}

pub(super) fn build_environment_context(cwd: &Path) -> String {
    let shell = std::env::var("SHELL")
        .ok()
        .or_else(|| std::env::var("COMSPEC").ok())
        .unwrap_or_else(|| "unknown".to_string());

    let shell = shell
        .rsplit(std::path::MAIN_SEPARATOR)
        .next()
        .unwrap_or(&shell)
        .to_lowercase();

    let current_date = chrono::Local::now().format("%Y-%m-%d").to_string();

    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string());

    format!(
        "<environment_context>\n  <cwd>{}</cwd>\n  <shell>{}</shell>\n  <current_date>{}</current_date>\n  <timezone>{}</timezone>\n</environment_context>",
        cwd.display(),
        shell,
        current_date,
        timezone,
    )
}

pub(super) fn build_prefetched_user_inputs(cwd: &Path) -> Vec<UserInput> {
    let mut inputs = Vec::new();
    if let Some(text) = load_prompt_md(cwd) {
        inputs.push(UserInput::Text {
            text,
            text_elements: Vec::new(),
        });
    }
    inputs.push(UserInput::Text {
        text: build_environment_context(cwd),
        text_elements: Vec::new(),
    });
    inputs
}

pub(super) fn append_prefetched_user_inputs(
    messages: &mut Vec<RequestMessage>,
    user_inputs: &[UserInput],
) {
    messages.splice(
        0..0,
        user_inputs.iter().filter_map(|input| match input {
            UserInput::Text { text, .. } if !text.trim().is_empty() => Some(RequestMessage {
                role: Role::User.as_str().to_string(),
                content: vec![RequestContent::Text { text: text.clone() }],
            }),
            UserInput::Text { .. }
            | UserInput::Image { .. }
            | UserInput::LocalImage { .. }
            | UserInput::Skill { .. }
            | UserInput::Mention { .. }
            | _ => None,
        }),
    );
}
