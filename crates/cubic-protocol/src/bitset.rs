use crate::{CodecError, CodecReader, CodecWriter, LengthKind};

/// Independent limits for the word count and highest usable bit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BitSetLimits {
    max_words: usize,
    max_bits: usize,
}

impl BitSetLimits {
    #[must_use]
    pub const fn new(max_words: usize, max_bits: usize) -> Self {
        Self {
            max_words,
            max_bits,
        }
    }

    #[must_use]
    pub const fn max_words(self) -> usize {
        self.max_words
    }

    #[must_use]
    pub const fn max_bits(self) -> usize {
        self.max_bits
    }

    const fn maximum_encoded_words(self) -> usize {
        let by_bits = self.max_bits.div_ceil(64);
        if by_bits < self.max_words {
            by_bits
        } else {
            self.max_words
        }
    }
}

/// Java-compatible variable BitSet stored as little-indexed 64-bit words.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    /// Constructs a bounded BitSet and removes trailing zero words.
    pub fn from_words(mut words: Vec<u64>, limits: BitSetLimits) -> Result<Self, CodecError> {
        validate_words(&words, limits)?;
        while words.last().is_some_and(|word| *word == 0) {
            words.pop();
        }
        Ok(Self { words })
    }

    #[must_use]
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    /// Safely queries a bit; absent words are false.
    #[must_use]
    pub fn is_set(&self, bit: usize) -> bool {
        self.words
            .get(bit / 64)
            .is_some_and(|word| word & (1_u64 << (bit % 64)) != 0)
    }

    pub(crate) fn decode(
        reader: &mut CodecReader<'_>,
        limits: BitSetLimits,
    ) -> Result<Self, CodecError> {
        let word_count = reader.read_length(LengthKind::BitSet)?;
        let max_words = limits.maximum_encoded_words();
        if word_count > max_words {
            return Err(CodecError::BitSetTooManyWords {
                words: word_count,
                max_words,
            });
        }
        let mut words = Vec::new();
        words
            .try_reserve_exact(word_count)
            .map_err(|_| CodecError::AllocationFailed {
                context: "BitSet words",
                requested: word_count,
            })?;
        for _ in 0..word_count {
            words.push(reader.read_u64()?);
        }
        Self::from_words(words, limits)
    }

    pub(crate) fn encode(
        &self,
        writer: &mut CodecWriter,
        limits: BitSetLimits,
    ) -> Result<(), CodecError> {
        validate_words(&self.words, limits)?;
        writer.write_length(self.words.len(), "BitSet word count")?;
        for word in &self.words {
            writer.write_u64(*word);
        }
        Ok(())
    }
}

fn validate_words(words: &[u64], limits: BitSetLimits) -> Result<(), CodecError> {
    let max_words = limits.maximum_encoded_words();
    if words.len() > max_words {
        return Err(CodecError::BitSetTooManyWords {
            words: words.len(),
            max_words,
        });
    }
    if let Some(bit) = highest_set_bit(words)
        && bit >= limits.max_bits()
    {
        return Err(CodecError::BitSetBitOutOfRange {
            bit,
            max_bits: limits.max_bits(),
        });
    }
    Ok(())
}

fn highest_set_bit(words: &[u64]) -> Option<usize> {
    words.iter().enumerate().rev().find_map(|(index, word)| {
        if *word == 0 {
            None
        } else {
            let within_word = 63_u32 - word.leading_zeros();
            index
                .checked_mul(64)
                .and_then(|base| base.checked_add(within_word as usize))
        }
    })
}
