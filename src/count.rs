use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct LineCounts {
    pub files: u64,
    pub blank: u64,
    pub comment: u64,
    pub code: u64,
}

impl LineCounts {
    pub fn add_assign(&mut self, other: Self) {
        self.files += other.files;
        self.blank += other.blank;
        self.comment += other.comment;
        self.code += other.code;
    }
}
