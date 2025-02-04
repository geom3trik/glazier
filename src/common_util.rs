// Copyright 2019 The Druid Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Common functions used by the backends

use std::cell::Cell;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::TryRecvError;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use instant::Instant;

use crate::kurbo::Point;
use crate::WinHandler;

// This is the default timing on windows.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(500);
// the max distance between two clicks for them to count as a multi-click
const MULTI_CLICK_MAX_DISTANCE: f64 = 5.0;

/// Strip the access keys from the menu string.
///
/// Changes "E&xit" to "Exit". Actual ampersands are escaped as "&&".
#[allow(dead_code)]
pub fn strip_access_key(raw_menu_text: &str) -> String {
    let mut saw_ampersand = false;
    let mut result = String::new();
    for c in raw_menu_text.chars() {
        if c == '&' {
            if saw_ampersand {
                result.push(c);
            }
            saw_ampersand = !saw_ampersand;
        } else {
            result.push(c);
            saw_ampersand = false;
        }
    }
    result
}

#[cfg_attr(feature = "wayland", allow(unused))]
pub(crate) type IdleCallback = Box<dyn for<'a> FnOnce(&'a mut dyn WinHandler) + Send>;

/// A sharable queue. Similar to a `std::sync::mpsc` channel, this queue is implemented as two types:
/// [`SharedEnqueuer`] and [`SharedDequeuer`].
///
/// # Comparison to `std::sync::mpsc::channel`
///
/// Similarities:
/// * The enqueuer is like a channel's `Sender` and the dequeuer is like a channel's `Receiver`.
/// * The enqueuer can be cloned and the dequeuer cannot be cloned.
/// * The queue is unbounded.
///
/// Differences:
/// * The dequeuer can only be polled to dequeue items. There is no blocking receive.
/// * The enqueuer can indicate when when the queue transitions from empty to non-empty, signaling
///   you wake the dequeue loop.
#[allow(dead_code)]
pub(crate) fn shared_queue<T>() -> (SharedEnqueuer<T>, SharedDequeuer<T>) {
    let (sender, receiver) = mpsc::channel();
    let empty_flag = Arc::new(Mutex::new(true));

    (
        SharedEnqueuer {
            sender,
            empty_flag: Arc::clone(&empty_flag),
        },
        SharedDequeuer {
            receiver,
            empty_flag,
        },
    )
}

/// A reference to a [`shared_queue`] that lets you enqueue callbacks.
#[allow(dead_code)]
pub(crate) struct SharedEnqueuer<T> {
    // NOTE: All sends must be done with the `empty_flag` lock held and updated.
    sender: mpsc::Sender<T>,
    empty_flag: Arc<Mutex<bool>>,
}

impl<T> SharedEnqueuer<T> {
    /// Adds a value to the queue. Returns `true` if the queue was empty before the value was added.
    /// In this case, you need to wake the dequeuer.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn enqueue(&self, t: T) -> bool {
        // Lock the empty flag before we send, otherwise it might become out of sync.
        let mut empty_flag = self.empty_flag.lock().unwrap();

        self.sender.send(t).unwrap();

        let was_empty = *empty_flag;
        *empty_flag = false;

        was_empty
    }
}

impl<T> Clone for SharedEnqueuer<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            empty_flag: Arc::clone(&self.empty_flag),
        }
    }
}

/// A reference to a [`shared_queue`] that lets you dequeue and consume callbacks.
#[allow(dead_code)]
pub(crate) struct SharedDequeuer<T> {
    // NOTE: All recieves must be done with the `empty_flag` lock held and updated.
    receiver: mpsc::Receiver<T>,
    empty_flag: Arc<Mutex<bool>>,
}

impl<T> SharedDequeuer<T> {
    #[allow(dead_code)]
    pub(crate) fn try_dequeue(&self) -> Option<T> {
        // Lock the empty flag before we receive, otherwise it might become out of sync.
        let mut empty_flag = self.empty_flag.lock().unwrap();

        let result = self.receiver.try_recv();

        if matches!(result, Err(TryRecvError::Empty)) {
            *empty_flag = true;
        }

        result.ok()
    }
}

impl<T> Iterator for SharedDequeuer<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.try_dequeue()
    }
}

/// An incrementing counter for generating unique ids.
///
/// This can be used safely from multiple threads.
///
/// The counter will overflow if `next()` is called 2^64 - 2 times.
/// If this is possible for your application, and reuse would be undesirable,
/// use something else.
pub struct Counter(AtomicU64);

impl Counter {
    /// Create a new counter.
    pub const fn new() -> Counter {
        Counter(AtomicU64::new(1))
    }

    /// Creates a new counter with a given starting value.
    ///
    /// # Safety
    ///
    /// The value must not be zero.
    pub const unsafe fn new_unchecked(init: u64) -> Counter {
        Counter(AtomicU64::new(init))
    }

    /// Return the next value.
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }

    /// Return the next value, as a `NonZeroU64`.
    pub fn next_nonzero(&self) -> NonZeroU64 {
        // unwrap won't happen because our initial value is 1 and can only be incremented.
        NonZeroU64::new(self.0.fetch_add(1, Ordering::Relaxed)).unwrap()
    }
}

/// A small helper for determining the click-count of a mouse-down event.
///
/// Click-count is incremented if both the duration and distance between a pair
/// of clicks are below some threshold.
#[derive(Debug, Clone)]
pub struct ClickCounter {
    max_interval: Cell<Duration>,
    max_distance: Cell<f64>,
    last_click: Cell<Instant>,
    last_pos: Cell<Point>,
    click_count: Cell<u8>,
}

#[allow(dead_code)]
impl ClickCounter {
    /// Create a new ClickCounter with the given interval and distance.
    pub fn new(max_interval: Duration, max_distance: f64) -> ClickCounter {
        ClickCounter {
            max_interval: Cell::new(max_interval),
            max_distance: Cell::new(max_distance),
            last_click: Cell::new(Instant::now()),
            click_count: Cell::new(0),
            last_pos: Cell::new(Point::new(f64::MAX, 0.0)),
        }
    }

    pub fn set_interval_ms(&self, millis: u64) {
        self.max_interval.set(Duration::from_millis(millis))
    }

    pub fn set_distance(&self, distance: f64) {
        self.max_distance.set(distance)
    }

    /// Return the click count for a click occurring now, at the provided position.
    pub fn count_for_click(&self, click_pos: Point) -> u8 {
        let click_time = Instant::now();
        let last_time = self.last_click.replace(click_time);
        let last_pos = self.last_pos.replace(click_pos);
        let elapsed = click_time - last_time;
        let distance = last_pos.distance(click_pos);
        if elapsed > self.max_interval.get() || distance > self.max_distance.get() {
            self.click_count.set(0);
        }
        let click_count = self.click_count.get().saturating_add(1);
        self.click_count.set(click_count);
        click_count
    }
}

impl Default for ClickCounter {
    fn default() -> Self {
        ClickCounter::new(MULTI_CLICK_INTERVAL, MULTI_CLICK_MAX_DISTANCE)
    }
}
