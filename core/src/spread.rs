//! Which pages of a book are shown together: a spread of one, two or three pages side by side. Without a screen: the viewer and
//! Peekel TV (an Android app on this core) share it; the picture of a spread is `view::side_by_side`.

/// How the pages of a book are put together on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spread {
    /// Pages side by side: 1, 2 or 3.
    pub count: usize,
    /// The first page stands alone (the cover), and the pairs are 2-3, 4-5; else they are 1-2, 3-4. For books whose scan lacks a blank
    /// page the reader shifts the pairs with `o`.
    pub cover_alone: bool,
    /// The pages are made as high as the tallest one.
    pub equal_height: bool,
}

impl Spread {
    /// Two pages, the cover alone, equally high: how a book is read (the defaults of the viewer's settings).
    pub const DEFAULT: Self = Self {
        count: 2,
        cover_alone: true,
        equal_height: true,
    };

    /// 2, then 3, then 1, then 2 again.
    #[must_use]
    pub fn next_count(self) -> Self {
        let count = match self.count {
            2 => 3,
            3 => 1,
            _ => 2,
        };
        Self { count, ..self }
    }

    /// The first page of the spread that page `page` of a file of many pages is in.
    pub fn start_of(self, page: usize) -> usize {
        if self.count <= 1 {
            page
        } else if self.cover_alone {
            if page == 0 {
                0
            } else {
                1 + (page - 1) / self.count * self.count
            }
        } else {
            page / self.count * self.count
        }
    }

    /// How many pages the spread that begins with page `start` has, in a file of `pages` (the last spread may have fewer).
    pub fn len_at(self, start: usize, pages: usize) -> usize {
        if self.count <= 1 || (self.cover_alone && start == 0) {
            1
        } else {
            self.count.min(pages.saturating_sub(start)).max(1)
        }
    }

    /// The first page of the spread before the one that begins with `start`, if there is one.
    pub fn before(self, start: usize) -> Option<usize> {
        start.checked_sub(1).map(|p| self.start_of(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_pages_with_the_cover_alone_go_1_then_2_3_then_4_5() {
        let s = Spread::DEFAULT;
        assert_eq!([0, 1, 2, 3, 4, 5].map(|p| s.start_of(p)), [0, 1, 1, 3, 3, 5]);
        assert_eq!(s.len_at(0, 6), 1);
        assert_eq!(s.len_at(1, 6), 2);
        assert_eq!(s.len_at(5, 6), 1, "an odd last page stays alone");
        assert_eq!(s.before(3), Some(1));
        assert_eq!(s.before(1), Some(0));
        assert_eq!(s.before(0), None);
    }

    #[test]
    fn shifted_the_pairs_are_1_2_then_3_4() {
        let s = Spread {
            cover_alone: false,
            ..Spread::DEFAULT
        };
        assert_eq!([0, 1, 2, 3, 4].map(|p| s.start_of(p)), [0, 0, 2, 2, 4]);
        assert_eq!(s.len_at(0, 5), 2);
        assert_eq!(s.len_at(4, 5), 1);
        assert_eq!(s.before(2), Some(0));
    }

    #[test]
    fn three_pages_and_one_page_and_the_order_of_the_counts() {
        let three = Spread {
            count: 3,
            ..Spread::DEFAULT
        };
        assert_eq!([0, 1, 2, 3, 4, 5].map(|p| three.start_of(p)), [0, 1, 1, 1, 4, 4]);
        assert_eq!(three.len_at(4, 6), 2);
        let one = Spread {
            count: 1,
            ..Spread::DEFAULT
        };
        assert_eq!(one.start_of(3), 3);
        assert_eq!(one.len_at(0, 6), 1);
        assert_eq!(Spread::DEFAULT.next_count().count, 3);
        assert_eq!(three.next_count().count, 1);
        assert_eq!(one.next_count().count, 2);
    }
}
