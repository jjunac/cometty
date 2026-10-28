#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};
use core::ops::Range;

use crate::{AttrsOwned, FamilyOwned, ShapeGlyph};
use core::hash::{Hash, Hasher};

/// Key for caching shape runs.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct ShapeRunKey {
    pub rtl: bool,
    pub text: String,
    pub default_attrs: AttrsOwned,
    pub attrs_spans: Vec<(Range<usize>, AttrsOwned)>,
}

/// A helper structure for caching shape runs.
#[derive(Default)]
pub struct ShapeRunCache {
    age: u64,
    cache: Vec<Entry>,
    payload_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Clone for ShapeRunCache {
    fn clone(&self) -> Self {
        // Vec::clone retains only len slots. Keep admission's full table
        // reservation, otherwise a later push could grow it outside the budget.
        let mut cloned = Self::default();
        cloned.set_limits(self.max_entries, self.max_bytes);
        cloned.age = self.age;
        for entry in &self.cache {
            cloned.insert(entry.key.clone(), entry.glyphs.clone());
            if let Some(last) = cloned.cache.last_mut() {
                last.age = entry.age;
            }
        }
        cloned
    }
}

#[derive(Clone)]
struct Entry {
    hash: u64,
    key: ShapeRunKey,
    age: u64,
    glyphs: Vec<ShapeGlyph>,
    bytes: usize,
}

fn attrs_bytes(attrs: &AttrsOwned) -> usize {
    let family = match &attrs.family_owned {
        // Conservatively charge even inline/shared names, including Arc header.
        FamilyOwned::Name(name) => name.len() + 2 * core::mem::size_of::<usize>(),
        _ => 0,
    };
    family + attrs.font_features.features.capacity() * core::mem::size_of::<crate::Feature>()
}

fn hash(key: &ShapeRunKey) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    key.hash(&mut hasher);
    hasher.finish()
}

impl ShapeRunCache {
    /// Configure a bounded generation cache. Zero disables it and releases storage.
    /// The budget charges vector capacities, text, attributes, and glyphs; it is
    /// not a bound on font resources, allocator overhead, or transient shaping.
    pub fn set_limits(&mut self, max_entries: usize, max_bytes: usize) {
        self.cache = Vec::new();
        self.payload_bytes = 0;
        self.max_entries = max_entries.min(max_bytes / core::mem::size_of::<Entry>());
        self.max_bytes = max_bytes;
        self.cache.reserve_exact(self.max_entries);
        if self.retained_bytes() > max_bytes {
            // Vec may reserve more than requested; never rely on its capacity
            // as the entry limit or admit an over-budget empty table.
            self.cache = Vec::new();
            self.max_entries = 0;
        }
    }

    pub fn clear(&mut self) {
        self.cache.clear();
        self.payload_bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    pub fn retained_bytes(&self) -> usize {
        self.cache.capacity() * core::mem::size_of::<Entry>() + self.payload_bytes
    }

    pub(crate) fn admits_text(&self, len: usize) -> bool {
        self.max_entries != 0 && len <= self.max_bytes / 4
    }

    /// Get cache item, updating age if found
    pub fn get(&mut self, key: &ShapeRunKey) -> Option<&Vec<ShapeGlyph>> {
        let hash = hash(key);
        self.cache
            .iter_mut()
            .rev()
            .find(|e| e.hash == hash && e.key == *key)
            .map(|entry| {
                entry.age = self.age;
                &entry.glyphs
            })
    }

    /// Insert cache item with current age
    pub fn insert(&mut self, key: ShapeRunKey, glyphs: Vec<ShapeGlyph>) {
        let bytes = key.text.capacity()
            + attrs_bytes(&key.default_attrs)
            + key.attrs_spans.capacity() * core::mem::size_of::<(Range<usize>, AttrsOwned)>()
            + key
                .attrs_spans
                .iter()
                .map(|(_, a)| attrs_bytes(a))
                .sum::<usize>()
            + glyphs.capacity() * core::mem::size_of::<ShapeGlyph>();
        // Reject giant entries, rather than letting one evict the working set.
        let table = self.cache.capacity() * core::mem::size_of::<Entry>();
        if self.max_entries == 0 || bytes > self.max_bytes.saturating_sub(table) / 4 {
            return;
        }
        if self.cache.len() == self.max_entries || self.retained_bytes() + bytes > self.max_bytes {
            self.clear();
        }
        self.payload_bytes += bytes;
        self.cache.push(Entry {
            hash: hash(&key),
            key,
            age: self.age,
            glyphs,
            bytes,
        });
    }

    /// Remove anything in the cache with an age older than `keep_ages`
    pub fn trim(&mut self, keep_ages: u64) {
        self.cache
            .retain(|entry| entry.age.saturating_add(keep_ages) >= self.age);
        self.payload_bytes = self.cache.iter().map(|entry| entry.bytes).sum();
        // Increase age
        self.age = self.age.saturating_add(1);
    }
}

impl core::fmt::Debug for ShapeRunCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("ShapeRunCache").finish()
    }
}
