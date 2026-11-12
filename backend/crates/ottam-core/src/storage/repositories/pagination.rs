/// Cursor pages respect both an item limit and an encoded-byte budget.
/// Repositories over-fetch by one row to detect continuation, then trim by
/// the byte budget; at least one item is always returned so oversized rows
/// cannot stall a traversal.
#[derive(Debug, Clone, Copy)]
pub struct PageRequest<'a> {
    pub limit: usize,
    pub byte_budget: usize,
    pub cursor: Option<&'a str>,
}

impl<'a> PageRequest<'a> {
    pub fn first(limit: usize, byte_budget: usize) -> Self {
        Self {
            limit,
            byte_budget,
            cursor: None,
        }
    }

    pub(crate) fn clamped_limit(&self) -> usize {
        self.limit.clamp(1, 500)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    pub fn map<U>(self, transform: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(transform).collect(),
            next_cursor: self.next_cursor,
        }
    }
}

/// Assemble a page from rows fetched newest-first (or in traversal order)
/// with one extra row beyond the limit. `weigh` approximates each item's
/// encoded size; `key` yields the cursor for continuing after an item.
pub(crate) fn assemble_page<T>(
    mut rows: Vec<T>,
    request: &PageRequest<'_>,
    weigh: impl Fn(&T) -> usize,
    key: impl Fn(&T) -> String,
) -> Page<T> {
    let limit = request.clamped_limit();
    let mut has_more = false;
    if rows.len() > limit {
        rows.truncate(limit);
        has_more = true;
    }
    let mut spent = 0usize;
    let mut kept = 0usize;
    for row in &rows {
        let weight = weigh(row);
        if kept > 0 && spent + weight > request.byte_budget {
            has_more = true;
            break;
        }
        spent += weight;
        kept += 1;
    }
    rows.truncate(kept);
    let next_cursor = if has_more {
        rows.last().map(&key)
    } else {
        None
    };
    Page {
        items: rows,
        next_cursor,
    }
}
