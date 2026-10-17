//! Page-local UTF-8 arenas. A cell needs two u32 offsets, not an owned String.
//! NULL has its own sentinel; an empty string remains a zero-length text span.
use std::collections::BTreeMap;
const PAGE_ROWS: usize = 1000;
const NULL: u32 = u32::MAX;
const LARGE: u32 = u32::MAX - 1;
#[derive(Clone, Copy, Debug)]
struct Span {
    offset: u32,
    len: u32,
}
#[derive(Debug)]
struct Page {
    text: String,
    cells: Vec<Span>,
    ends: Vec<usize>,
    // Defensive escape hatch for a single >4 GiB cell/page, without truncation.
    large: BTreeMap<usize, String>,
}
impl Page {
    fn new(rows: Vec<Vec<Option<String>>>) -> Self {
        let bytes = rows
            .iter()
            .flatten()
            .flatten()
            .map(String::len)
            .sum::<usize>();
        let cells = rows.iter().map(Vec::len).sum();
        let mut p = Self {
            text: String::with_capacity(bytes.min(LARGE as usize)),
            cells: Vec::with_capacity(cells),
            ends: Vec::with_capacity(rows.len() + 1),
            large: BTreeMap::new(),
        };
        p.ends.push(0);
        for row in rows {
            for cell in row {
                let at = p.cells.len();
                let span = p.append(at, cell);
                p.cells.push(span);
            }
            p.ends.push(p.cells.len());
        }
        p
    }
    fn append(&mut self, at: usize, value: Option<String>) -> Span {
        let Some(value) = value else {
            return Span {
                offset: 0,
                len: NULL,
            };
        };
        if value.len() >= LARGE as usize
            || self.text.len().saturating_add(value.len()) >= LARGE as usize
        {
            self.large.insert(at, value);
            return Span {
                offset: 0,
                len: LARGE,
            };
        }
        let offset = self.text.len() as u32;
        let len = value.len() as u32;
        self.text.push_str(&value);
        Span { offset, len }
    }
    fn cell(&self, at: usize) -> Option<&str> {
        let s = self.cells[at];
        match s.len {
            NULL => None,
            LARGE => self.large.get(&at).map(String::as_str),
            _ => Some(&self.text[s.offset as usize..s.offset as usize + s.len as usize]),
        }
    }
    fn owned_row(&self, row: usize) -> Vec<Option<String>> {
        (self.ends[row]..self.ends[row + 1])
            .map(|i| self.cell(i).map(str::to_owned))
            .collect()
    }
}
#[derive(Debug, Default)]
pub struct RowStore {
    pages: Vec<Page>,
    len: usize,
}
#[derive(Clone, Copy)]
pub struct Row<'a> {
    page: &'a Page,
    start: usize,
    end: usize,
}
impl<'a> Row<'a> {
    pub fn len(&self) -> usize {
        self.end - self.start
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
    pub fn get(&self, col: usize) -> Option<Option<&'a str>> {
        (col < self.len()).then(|| self.page.cell(self.start + col))
    }
    pub fn to_owned(&self) -> Vec<Option<String>> {
        (self.start..self.end)
            .map(|i| self.page.cell(i).map(str::to_owned))
            .collect()
    }
}
impl RowStore {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn get(&self, row: usize) -> Option<Row<'_>> {
        if row >= self.len {
            return None;
        }
        let p = &self.pages[row / PAGE_ROWS];
        let r = row % PAGE_ROWS;
        Some(Row {
            page: p,
            start: p.ends[r],
            end: p.ends[r + 1],
        })
    }
    pub fn cell(&self, row: usize, col: usize) -> Option<&str> {
        self.get(row).and_then(|r| r.get(col)).flatten()
    }
    pub fn iter(&self) -> impl Iterator<Item = Row<'_>> {
        (0..self.len).map(|r| self.get(r).unwrap())
    }
    pub fn set(&mut self, row: usize, col: usize, value: Option<String>) {
        if row >= self.len {
            return;
        }
        let p = &mut self.pages[row / PAGE_ROWS];
        let r = row % PAGE_ROWS;
        let Some(at) = p.ends[r].checked_add(col) else {
            return;
        };
        if at >= p.ends[r + 1] {
            return;
        }
        p.large.remove(&at);
        p.cells[at] = p.append(at, value);
        // Bound stale text from repeated edits. Rebuild only this 1,000-row page.
        let live = p
            .cells
            .iter()
            .filter(|s| s.len < LARGE)
            .map(|s| s.len as usize)
            .sum::<usize>();
        if p.text.len() > live.saturating_mul(2).saturating_add(4096) {
            *p = Page::new((0..p.ends.len() - 1).map(|r| p.owned_row(r)).collect());
        }
    }
    pub fn replace_row(&mut self, row: usize, values: Vec<Option<String>>) {
        if row >= self.len {
            return;
        }
        let p = &mut self.pages[row / PAGE_ROWS];
        let mut rows: Vec<_> = (0..p.ends.len() - 1).map(|r| p.owned_row(r)).collect();
        rows[row % PAGE_ROWS] = values;
        *p = Page::new(rows);
    }
    pub fn push(&mut self, row: Vec<Option<String>>) {
        if self.len % PAGE_ROWS == 0 {
            self.pages.push(Page::new(vec![row]));
        } else {
            let p = self.pages.last_mut().unwrap();
            for cell in row {
                let span = p.append(p.cells.len(), cell);
                p.cells.push(span);
            }
            p.ends.push(p.cells.len());
        }
        self.len += 1;
    }
    pub fn storage_bytes(&self) -> usize {
        self.pages.capacity() * std::mem::size_of::<Page>()
            + self
                .pages
                .iter()
                .map(|p| {
                    p.text.capacity()
                        + p.cells.capacity() * std::mem::size_of::<Span>()
                        + p.ends.capacity() * std::mem::size_of::<usize>()
                        + p.large.values().map(String::capacity).sum::<usize>()
                })
                .sum::<usize>()
    }
}
impl From<Vec<Vec<Option<String>>>> for RowStore {
    fn from(rows: Vec<Vec<Option<String>>>) -> Self {
        rows.into_iter().collect()
    }
}
impl FromIterator<Vec<Option<String>>> for RowStore {
    fn from_iter<I: IntoIterator<Item = Vec<Option<String>>>>(iter: I) -> Self {
        let mut store = Self::default();
        let mut batch = Vec::with_capacity(PAGE_ROWS);
        for row in iter {
            batch.push(row);
            store.len += 1;
            if batch.len() == PAGE_ROWS {
                store.pages.push(Page::new(std::mem::replace(
                    &mut batch,
                    Vec::with_capacity(PAGE_ROWS),
                )));
            }
        }
        if !batch.is_empty() {
            store.pages.push(Page::new(batch));
        }
        store
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arenas_preserve_null_empty_utf8_ragged_rows_and_page_boundaries() {
        let rows: Vec<_> = (0..2001)
            .map(|i| vec![Some(format!("ธนาคาร😺 {i}")), None, Some(String::new())])
            .collect();
        let mut store: RowStore = rows.clone().into();
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(store.get(i).unwrap().to_owned(), *row);
        }
        store.set(1000, 1, Some("NULL".into()));
        store.replace_row(999, vec![Some("DEFAULT".into())]);
        for i in 0..1000 {
            store.set(0, 0, Some(format!("{} {i}", "😺".repeat(100))));
        }
        store.set(1, usize::MAX, Some("ignored".into()));
        store.set(usize::MAX, 0, None);
        store.push(vec![]);
        assert_eq!(store.cell(1000, 1), Some("NULL"));
        assert_eq!(store.get(999).unwrap().len(), 1);
        assert!(store.get(2001).unwrap().is_empty());
        assert!(store.storage_bytes() < 250_000);
    }
}
