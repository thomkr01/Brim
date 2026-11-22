//! The one leave guard: a page with unsaved edits registers itself with [`guard`], and leaving it
//! (another page, the page's own Back button, a closed tab) asks to save, discard or stay.
use crate::common::{ask, Ask, ALT};
use leptos::prelude::*;
use std::{cell::{Cell, RefCell}, rc::Rc, time::Duration};

struct G {
    dirty: Box<dyn Fn() -> bool>,
    /// True while a save runs.
    busy: Box<dyn Fn() -> bool>,
    /// What must be fixed before the edits can be saved.
    problem: Box<dyn Fn() -> Option<String>>,
    save: Box<dyn Fn()>,
}

thread_local! {
    static LIVE: RefCell<Vec<(usize, Rc<G>)>> = const { RefCell::new(Vec::new()) };
    static NEXT: Cell<usize> = const { Cell::new(0) };
    /// Set when the answer was "leave": the next page change goes through.
    static OPEN: Cell<bool> = const { Cell::new(false) };
}

/// Guards the page's edits until the page goes. `save` starts the page's own save, which shows its
/// own errors; the guard leaves once `dirty` is off, or stays if the save did not take.
pub fn guard(
    dirty: impl Fn() -> bool + 'static,
    busy: impl Fn() -> bool + 'static,
    problem: impl Fn() -> Option<String> + 'static,
    save: impl Fn() + 'static,
) {
    let id = NEXT.get();
    NEXT.set(id + 1);
    let g = Rc::new(G { dirty: Box::new(dirty), busy: Box::new(busy), problem: Box::new(problem), save: Box::new(save) });
    LIVE.with_borrow_mut(|l| l.push((id, g.clone())));
    on_cleanup(move || LIVE.with_borrow_mut(|l| l.retain(|(i, _)| *i != id)));
    // A closed or reloaded tab: the browser asks itself.
    let _ = window_event_listener(leptos::ev::beforeunload, move |e| {
        if (g.dirty)() {
            e.prevent_default()
        }
    });
}

fn dirty() -> Vec<Rc<G>> {
    LIVE.with_borrow(|l| l.iter().filter(|(_, g)| (g.dirty)()).map(|(_, g)| g.clone()).collect())
}

/// Runs `then` at once when nothing is unsaved; else asks first: Save changes, Discard changes or Cancel.
pub fn leave(then: impl FnOnce() + 'static) {
    let live = dirty();
    if live.is_empty() {
        return then();
    }
    let problem = live.iter().find_map(|g| (g.problem)());
    let a = Ask::new("Unsaved changes");
    let a = match &problem {
        Some(p) => a.text(format!("{p} Fix that first, then save. Or discard your changes.")).yes(""),
        None => a.text("Save your changes before you leave, or discard them.").yes("Save changes"),
    };
    ask(a.alt("Discard changes"), move |answer| {
        if answer == ALT {
            return then();
        }
        for g in &live {
            (g.save)();
        }
        settle(live, Box::new(then), 0);
    });
}

/// Waits for the saves to end (up to 10 s), then leaves if nothing is left to save. A save that
/// did not start (a question, a 2FA code) or failed keeps the person on the page.
fn settle(live: Vec<Rc<G>>, then: Box<dyn FnOnce()>, tries: u8) {
    set_timeout(
        move || {
            if tries < 100 && live.iter().any(|g| (g.busy)()) {
                return settle(live, then, tries + 1);
            }
            if !live.iter().any(|g| (g.dirty)()) {
                then()
            }
        },
        Duration::from_millis(100),
    );
}

/// For the router: true when a page change must wait for the answer. The address bar is put back
/// at once; the page it asked for opens after Save changes or Discard changes.
pub fn hold(e: &web_sys::HashChangeEvent) -> bool {
    if OPEN.replace(false) || dirty().is_empty() {
        return false;
    }
    let w = web_sys::window().unwrap();
    let _ = w.history().and_then(|h| h.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&e.old_url())));
    let to = e.new_url().split_once('#').map_or(String::new(), |(_, h)| h.to_string());
    // After this event's other listeners: the dialog closes itself on a page change.
    set_timeout(
        move || {
            leave(move || {
                OPEN.set(true);
                let _ = web_sys::window().unwrap().location().set_hash(&to);
            })
        },
        Duration::ZERO,
    );
    true
}
