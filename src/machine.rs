//! Fake machine implementation to validate an allocation history.

use std::{
    collections::{btree_map as map, BTreeMap},
    fmt,
};

use crate::{Event, Pointer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    ConflictingAlloc { requested: Region, existing: Region },
    MisalignedAlloc { requested: Region },
    IncompleteFree { requested: Region, existing: Region },
    MisalignedFree { requested: Region, existing: Region },
    MissingFree { requested: Region },
    Leaked { region: Region },
}

impl Violation {
    /// Test that this violation refers to a dangling region and that it matches
    /// the given predicate.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use checkers::{Region, Violation};
    /// let violation = Violation::Leaked {
    ///     region: Region::new(42.into(), 20, 4),
    /// };
    /// assert!(violation.is_leaked_with(|r| r.size == 20 && r.align == 4));
    ///
    /// let requested = Region::new(10.into(), 10, 1);
    /// let violation = Violation::MisalignedAlloc { requested };
    /// assert!(!violation.is_leaked_with(|r| true));
    /// ```
    pub fn is_leaked_with<F>(&self, f: F) -> bool
    where
        F: FnOnce(Region) -> bool,
    {
        match *self {
            Self::Leaked { region } => f(region),
            _ => false,
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConflictingAlloc {
                requested,
                existing,
            } => write!(
                fmt,
                "Requested allocation ({}) overlaps with existing ({})",
                requested, existing
            ),
            Self::MisalignedAlloc { requested } => {
                write!(fmt, "Allocated region ({}) is misaligned.", requested)
            }
            Self::IncompleteFree {
                requested,
                existing,
            } => write!(
                fmt,
                "Freed ({}) only part of existing region ({})",
                requested, existing
            ),
            Self::MisalignedFree {
                requested,
                existing,
            } => write!(
                fmt,
                "Freed region ({}) has different alignment from existing ({})",
                requested, existing
            ),
            Self::MissingFree { requested } => write!(fmt, "Freed missing region ({})", requested),
            Self::Leaked { region } => write!(fmt, "Dangling region ({})", region),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Region {
    /// The pointer of the allocation.
    pub ptr: Pointer,
    /// The size of the allocation.
    pub size: usize,
    /// The alignment of the allocation.
    pub align: usize,
}

impl Region {
    pub fn new(ptr: Pointer, size: usize, align: usize) -> Self {
        Self { ptr, size, align }
    }

    /// Test if this region overlaps with another region.
    pub fn overlaps(self, other: Self) -> bool {
        self.ptr <= other.ptr && other.ptr < self.ptr.saturating_add(self.size)
    }

    /// Test if regions are the same (minus alignment).
    pub fn is_same_region_as(self, other: Self) -> bool {
        self.ptr == other.ptr && self.size == other.size
    }
}

impl fmt::Display for Region {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            fmt,
            "{}-{} (size: {}, align: {})",
            self.ptr,
            self.ptr.saturating_add(self.size),
            self.size,
            self.align,
        )
    }
}

/// Fake machine implementation to validate an allocation history.
#[derive(Default)]
pub struct Machine {
    /// Used memory regions.
    regions: BTreeMap<Pointer, Region>,
    /// Current memory used according to allocations.
    pub memory_used: usize,
}

impl Machine {
    /// Push an event into the machine.
    ///
    /// # Examples
    ///
    /// Checks for a double-free:
    ///
    /// ```rust
    /// use checkers::{Event::*, Region, Machine};
    ///
    /// let mut machine = Machine::default();
    ///
    /// assert!(machine.push(Alloc(Region::new(0.into(), 2, 1))).is_ok());
    /// assert!(machine.push(Free(Region::new(0.into(), 2, 1))).is_ok());
    /// assert!(machine.push(Free(Region::new(0.into(), 2, 1))).is_err());
    /// ```
    ///
    /// Check for a misaligned allocation:
    ///
    /// ```rust
    /// use checkers::{Event::*, Region, Machine, Violation};
    ///
    /// let mut machine = Machine::default();
    /// let requested = Region::new(5.into(), 2, 4);
    ///
    /// assert_eq!(
    ///     Err(Violation::MisalignedAlloc { requested }),
    ///     machine.push(Alloc(requested))
    /// );
    /// ```
    ///
    /// Tries to deallocate part of other region:
    ///
    /// ```rust
    /// use checkers::{Event::*, Region, Machine, Violation};
    ///
    /// let mut machine = Machine::default();
    /// let existing = Region::new(100.into(), 100, 1);
    ///
    /// assert!(machine.push(Alloc(existing)).is_ok());
    ///
    /// let requested = Region::new(150.into(), 50, 1);
    /// assert_eq!(
    ///     Err(Violation::MissingFree { requested }),
    ///     machine.push(Free(requested))
    /// );
    ///
    /// let requested = Region::new(100.into(), 50, 1);
    /// assert_eq!(
    ///     Err(Violation::IncompleteFree { requested, existing }),
    ///     machine.push(Free(requested))
    /// );
    /// ```
    pub fn push(&mut self, event: Event) -> Result<(), Violation> {
        match event {
            Event::Alloc(requested) => {
                if !requested.ptr.is_aligned_with(requested.align) {
                    return Err(Violation::MisalignedAlloc { requested });
                }

                if let Some(existing) = find_region_overlaps(&self.regions, requested).next() {
                    return Err(Violation::ConflictingAlloc {
                        requested,
                        existing,
                    });
                }

                self.memory_used = self.memory_used.saturating_add(requested.size);
                debug_assert!(self.regions.insert(requested.ptr, requested).is_none());
            }
            Event::Free(requested) => {
                if let map::Entry::Occupied(entry) = self.regions.entry(requested.ptr) {
                    let existing = *entry.get();

                    if !existing.is_same_region_as(requested) {
                        return Err(Violation::IncompleteFree {
                            requested,
                            existing,
                        });
                    }

                    if existing.align != requested.align {
                        return Err(Violation::MisalignedFree {
                            requested,
                            existing,
                        });
                    }

                    let (_, region) = entry.remove_entry();
                    self.memory_used = self.memory_used.saturating_sub(region.size);
                    return Ok(());
                }

                return Err(Violation::MissingFree { requested });
            }
        }

        return Ok(());

        fn find_region_overlaps<'a>(
            regions: &'a BTreeMap<Pointer, Region>,
            needle: Region,
        ) -> impl Iterator<Item = Region> + 'a {
            let head = regions
                .range(..=needle.ptr)
                .take_while(move |(_, &r)| r.overlaps(needle));

            let tail = regions
                .range(needle.ptr..)
                .take_while(move |(_, &r)| r.overlaps(needle));

            head.chain(tail).map(|(_, &r)| r)
        }
    }

    /// Access all trailing regions (ones which have not been deallocated).
    pub fn trailing_regions(&self) -> Vec<Region> {
        self.regions.values().copied().collect()
    }
}
