#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatchKind {
    Add,
    Update,
    Delete,
    Move,
}

impl PatchKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Move => "move",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PatchChange {
    pub(crate) path: String,
    pub(crate) move_path: Option<String>,
    pub(crate) content: String,
    pub(crate) hunks: Vec<PatchHunk>,
    pub(crate) kind: PatchKind,
}

#[derive(Debug, Clone)]
pub(crate) struct PatchHunk {
    pub(crate) lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HunkLine {
    Context(String),
    Add(String),
    Remove(String),
}
