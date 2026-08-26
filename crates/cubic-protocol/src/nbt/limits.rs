/// Explicit per-value and cumulative limits for untrusted Java Edition NBT.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NbtLimits {
    max_depth: usize,
    max_total_tags: usize,
    max_compound_entries: usize,
    max_list_elements: usize,
    max_array_elements: usize,
    max_string_encoded_bytes: usize,
    max_total_allocated_bytes: usize,
}

impl NbtLimits {
    pub const DEFAULT_MAX_DEPTH: usize = 64;
    pub const DEFAULT_MAX_TOTAL_TAGS: usize = 65_536;
    pub const DEFAULT_MAX_COMPOUND_ENTRIES: usize = 4_096;
    pub const DEFAULT_MAX_LIST_ELEMENTS: usize = 65_536;
    pub const DEFAULT_MAX_ARRAY_ELEMENTS: usize = 1_048_576;
    pub const DEFAULT_MAX_STRING_ENCODED_BYTES: usize = u16::MAX as usize;
    pub const DEFAULT_MAX_TOTAL_ALLOCATED_BYTES: usize = 16 * 1024 * 1024;

    #[must_use]
    pub const fn max_depth(self) -> usize {
        self.max_depth
    }

    #[must_use]
    pub const fn max_total_tags(self) -> usize {
        self.max_total_tags
    }

    #[must_use]
    pub const fn max_compound_entries(self) -> usize {
        self.max_compound_entries
    }

    #[must_use]
    pub const fn max_list_elements(self) -> usize {
        self.max_list_elements
    }

    #[must_use]
    pub const fn max_array_elements(self) -> usize {
        self.max_array_elements
    }

    #[must_use]
    pub const fn max_string_encoded_bytes(self) -> usize {
        if self.max_string_encoded_bytes < u16::MAX as usize {
            self.max_string_encoded_bytes
        } else {
            u16::MAX as usize
        }
    }

    #[must_use]
    pub const fn max_total_allocated_bytes(self) -> usize {
        self.max_total_allocated_bytes
    }

    #[must_use]
    pub const fn with_max_depth(mut self, value: usize) -> Self {
        self.max_depth = value;
        self
    }

    #[must_use]
    pub const fn with_max_total_tags(mut self, value: usize) -> Self {
        self.max_total_tags = value;
        self
    }

    #[must_use]
    pub const fn with_max_compound_entries(mut self, value: usize) -> Self {
        self.max_compound_entries = value;
        self
    }

    #[must_use]
    pub const fn with_max_list_elements(mut self, value: usize) -> Self {
        self.max_list_elements = value;
        self
    }

    #[must_use]
    pub const fn with_max_array_elements(mut self, value: usize) -> Self {
        self.max_array_elements = value;
        self
    }

    #[must_use]
    pub const fn with_max_string_encoded_bytes(mut self, value: usize) -> Self {
        self.max_string_encoded_bytes = value;
        self
    }

    #[must_use]
    pub const fn with_max_total_allocated_bytes(mut self, value: usize) -> Self {
        self.max_total_allocated_bytes = value;
        self
    }
}

impl Default for NbtLimits {
    fn default() -> Self {
        Self {
            max_depth: Self::DEFAULT_MAX_DEPTH,
            max_total_tags: Self::DEFAULT_MAX_TOTAL_TAGS,
            max_compound_entries: Self::DEFAULT_MAX_COMPOUND_ENTRIES,
            max_list_elements: Self::DEFAULT_MAX_LIST_ELEMENTS,
            max_array_elements: Self::DEFAULT_MAX_ARRAY_ELEMENTS,
            max_string_encoded_bytes: Self::DEFAULT_MAX_STRING_ENCODED_BYTES,
            max_total_allocated_bytes: Self::DEFAULT_MAX_TOTAL_ALLOCATED_BYTES,
        }
    }
}
