use std::ops::Range;

pub(crate) struct CatalogData {
    pub bytes: &'static [u8],
}

#[derive(Clone, Copy)]
pub(crate) struct ProviderData {
    pub id: &'static str,
    pub fingerprint: &'static str,
    pub first: usize,
    pub end: usize,
    pub bytes: &'static [u8],
}

pub(crate) struct ModelData {
    pub provider: usize,
    pub id: &'static str,
    pub tokens: Vec<&'static str>,
    pub tag: Option<&'static str>,
    pub bytes: &'static [u8],
    pub canonical: Vec<&'static str>,
    pub display: Vec<&'static str>,
    pub name: &'static str,
    pub label: Option<&'static str>,
}

impl CatalogData {
    fn number(&self, offset: usize) -> usize {
        u32::from_le_bytes(self.bytes[offset..offset + 4].try_into().unwrap()) as usize
    }

    fn span(&self, offset: usize) -> &'static [u8] {
        let start = self.number(offset);
        &self.bytes[start..start + self.number(offset + 4)]
    }

    fn text(&self, offset: usize) -> &'static str {
        std::str::from_utf8(self.span(offset)).expect("build-time UTF-8")
    }

    pub fn provider(&self, index: usize) -> ProviderData {
        let at = self.number(0) + index * 32;
        ProviderData {
            id: self.text(at),
            fingerprint: self.text(at + 8),
            first: self.number(at + 16),
            end: self.number(at + 20),
            bytes: self.span(at + 24),
        }
    }

    pub fn provider_named(&self, id: &str) -> Option<ProviderData> {
        self.matching(&format!("p\0{id}"))
            .next()
            .map(|index| self.provider(index))
    }

    pub fn providers(&self) -> impl Iterator<Item = ProviderData> + '_ {
        (0..self.number(4)).map(|index| self.provider(index))
    }

    pub fn model(&self, index: usize) -> ModelData {
        let at = self.number(8) + index * 68;
        let tag = self.text(at + 20);
        let label = self.text(at + 36);
        ModelData {
            provider: self.number(at),
            id: self.text(at + 4),
            tokens: self
                .text(at + 12)
                .split('\0')
                .filter(|s| !s.is_empty())
                .collect(),
            tag: (!tag.is_empty()).then_some(tag),
            bytes: self.span(at + 28),
            canonical: self
                .text(at + 44)
                .split('\0')
                .filter(|s| !s.is_empty())
                .collect(),
            display: self
                .text(at + 52)
                .split('\0')
                .filter(|s| !s.is_empty())
                .collect(),
            name: self.text(self.number(24) + index * 8),
            label: (!label.is_empty()).then_some(label),
        }
    }

    fn key(&self, index: usize) -> &'static str {
        self.text(self.number(16) + index * 12)
    }

    fn bound(&self, key: &str, upper: bool) -> usize {
        let (mut lo, mut hi) = (0, self.number(20));
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.key(mid) < key || (upper && self.key(mid) == key) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    pub fn matching<'a>(&'a self, key: &str) -> impl Iterator<Item = usize> + use<'a> {
        let range = self.bound(key, false)..self.bound(key, true);
        self.positions(range)
    }

    fn positions(&self, range: Range<usize>) -> impl Iterator<Item = usize> + '_ {
        range.map(|index| self.number(self.number(16) + index * 12 + 8))
    }

    pub fn targets<'a>(&'a self, provider: &str, after: &str) -> impl Iterator<Item = usize> + 'a {
        let prefix = format!("t\0{provider}\0");
        let start = self.bound(&format!("{prefix}{after}"), !after.is_empty());
        let end = self.bound(&format!("t\0{provider}\u{1}"), false);
        self.positions(start..end)
    }
}
