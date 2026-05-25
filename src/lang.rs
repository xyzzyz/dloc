#[derive(Debug)]
pub struct LanguageRegistry;

impl LanguageRegistry {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}
