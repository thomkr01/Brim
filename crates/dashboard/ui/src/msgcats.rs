//! Saved messages in the server's own categories: one card row per category (closed at first, as
//! the Bot messages areas), "Uncategorized" last. Rows and categories sort A to Z until someone
//! drags one (the shared `Grip`); "Sort A to Z" goes back. A message dragged onto another category's
//! row, or onto its title, moves there.
use crate::common::*;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{json, Value};
use std::collections::HashSet;

/// The category of a message as the page groups it: "" for Uncategorized (also for a category the
/// list no longer has).
fn home(m: &Value, cats: &[Value]) -> String {
    let c = m["category"].as_str().unwrap_or("");
    if cats.iter().any(|x| x["id"] == c) {
        c.to_string()
    } else {
        String::new()
    }
}

/// Messages top down as the page shows them: by category order, Uncategorized last.
fn flat(rows: &[Value], cats: &[Value]) -> Vec<(String, String)> {
    let ids = cats.iter().map(|c| c["id"].as_str().unwrap_or("").to_string()).chain([String::new()]);
    ids.flat_map(|c| {
        rows.iter()
            .filter(|m| home(m, cats) == c)
            .map(|m| (m["id"].as_i64().unwrap_or(0).to_string(), c.clone()))
            .collect::<Vec<_>>()
    })
    .collect()
}

/// Messages top down with their category, and the category ids top down.
type Order = (Vec<(String, String)>, Vec<String>);

/// A move of `from` to `to`. Places below `n` are messages; `n + k` is category `k`'s title. A
/// message dropped on a title goes to the end of that category; a title dropped on a title moves
/// the category. None when nothing changes.
pub fn moved(
    mut order: Vec<(String, String)>,
    mut cats: Vec<String>,
    (from, to): (usize, usize),
) -> Option<Order> {
    let n = order.len();
    if from == to || from >= n + cats.len() || to >= n + cats.len() {
        return None;
    }
    match (from < n, to < n) {
        (true, true) => {
            let mut m = order.remove(from);
            // The target row sits one place higher once the dragged row is out (when dragged down).
            m.1 = order.get(if from < to { to - 1 } else { to }).map_or(m.1.clone(), |o| o.1.clone());
            // The dropped row takes the target's place: above it when dragged up, below when down.
            order.insert(to, m);
        }
        (true, false) => {
            let mut m = order.remove(from);
            m.1 = cats[to - n].clone();
            order.push(m);
        }
        (false, false) => {
            let c = cats.remove(from - n);
            cats.insert(to - n, c);
        }
        (false, true) => return None,
    }
    Some((order, cats))
}

/// An arrow key on message `from`: one place up or down inside its category; past the edge it goes
/// to the end (up) or the start (down) of the next category on the page, also an empty one.
pub fn step(mut order: Vec<(String, String)>, cats: Vec<String>, from: usize, down: bool) -> Option<Order> {
    let mut m = order.get(from)?.clone();
    let seq: Vec<String> = cats.iter().cloned().chain([String::new()]).collect();
    let same: Vec<usize> = (0..order.len()).filter(|&i| order[i].1 == m.1).collect();
    let k = same.iter().position(|&i| i == from)?;
    let next = if down { same.get(k + 1) } else { k.checked_sub(1).and_then(|k| same.get(k)) };
    if let Some(&to) = next {
        order.swap(from, to);
        return Some((order, cats));
    }
    let c = seq.iter().position(|c| *c == m.1)?;
    let c = if down { c + 1 } else { c.checked_sub(1)? };
    m.1 = seq.get(c)?.clone();
    order.remove(from);
    let at = if down {
        order.iter().position(|o| o.1 == m.1).unwrap_or(order.len())
    } else {
        order.iter().rposition(|o| o.1 == m.1).map_or(order.len(), |i| i + 1)
    };
    order.insert(at, m);
    // Keep the list grouped as the page shows it (an empty category had no place yet).
    order.sort_by_key(|o| seq.iter().position(|c| *c == o.1));
    Some((order, cats))
}

/// The grouped list. `row` draws one message row; `changed` reloads the page's lists.
#[component]
pub fn Grouped(
    guild: StoredValue<String>,
    rows: RwSignal<Vec<Value>>,
    cats: RwSignal<Vec<Value>>,
    q: RwSignal<String>,
    row: Callback<Value, AnyView>,
    changed: Callback<()>,
    msg: Msg,
) -> impl IntoView {
    let opened = RwSignal::new(HashSet::<String>::new());
    let drag = RwSignal::new(None::<usize>);
    let over = RwSignal::new(None::<usize>);
    // What a screen reader hears after a move.
    let said = RwSignal::new(String::new());
    let base = move || format!("/api/guilds/{}/message-categories", guild.get_value());
    // `key`: an arrow key on a message (Some(down)), else a drop of `from` on `to`.
    let go = move |from: usize, to: usize, key: Option<bool>| {
        let ids = cats.with_untracked(|c| c.iter().map(|x| x["id"].as_str().unwrap_or("").to_string()).collect());
        let order = rows.with_untracked(|r| cats.with_untracked(|c| flat(r, c)));
        let n = order.len();
        let who = order.get(from).map(|m| m.0.clone());
        let new = match key {
            Some(down) => step(order, ids, from, down),
            None => moved(order, ids, (from, to)),
        };
        let Some((order, ids)) = new else { return };
        let new_i = match &who {
            Some(w) if from < n => order.iter().position(|m| &m.0 == w).unwrap_or(0),
            _ => to,
        };
        // The place a person sees: among the categories, or inside the message's category.
        let at = if from < n { order[..new_i].iter().filter(|m| m.1 == order[new_i].1).count() + 1 } else { to - n + 1 };
        spawn_local(async move {
            let body = json!({"categories": ids, "messages": order});
            match api("POST", &format!("{}/order", base()), Some(body)).await {
                Ok(_) => {
                    // The page takes the new order itself (no reload), then puts focus back on the moved row.
                    let pos = |id: &str| order.iter().position(|m| m.0 == id);
                    rows.update(|r| {
                        for m in r.iter_mut() {
                            let id = m["id"].as_i64().unwrap_or(0).to_string();
                            if let Some(p) = pos(&id) {
                                m["category"] = if order[p].1.is_empty() { Value::Null } else { json!(order[p].1) };
                                m["position"] = json!(p + 1);
                            }
                        }
                        r.sort_by_key(|m| pos(&m["id"].as_i64().unwrap_or(0).to_string()).unwrap_or(usize::MAX));
                    });
                    cats.update(|c| {
                        for x in c.iter_mut() {
                            x["position"] = json!(ids.iter().position(|i| x["id"] == i.as_str()).map_or(0, |p| p + 1));
                        }
                        c.sort_by_key(|x| x["position"].as_u64().unwrap_or(u64::MAX));
                    });
                    if from < n {
                        // The category the row went to opens, so the row stays in view.
                        let c = &order[new_i].1;
                        opened.update(|o| { o.insert(if c.is_empty() { "-".to_string() } else { c.clone() }); });
                    }
                    said.set(format!("Moved to place {at}."));
                    // Its place on the page now (grouped by category), for the focus.
                    let shown_i = match &who {
                        Some(w) if from < n => rows.with_untracked(|r| cats.with_untracked(|c| flat(r, c))).iter().position(|m| &m.0 == w),
                        _ => Some(to),
                    };
                    let Some(new_i) = shown_i else { return };
                    set_timeout(
                        move || {
                            let sel = format!(".msort[data-i=\"{new_i}\"], .mcathead[data-i=\"{new_i}\"] button");
                            if let Some(el) = document().query_selector(&sel).ok().flatten() {
                                let _ = wasm_bindgen::JsCast::unchecked_into::<web_sys::HtmlElement>(el).focus();
                            }
                        },
                        std::time::Duration::ZERO,
                    );
                }
                Err(e) => msg.err(e),
            }
        });
    };
    let place = move |(from, to): (usize, usize)| go(from, to, None);
    let rename = move |id: String, name: String| {
        ask(Ask::new("Rename category").yes("Rename").input(name), move |to| {
            spawn_local(async move {
                match api("POST", &format!("{}/{id}/rename", base()), Some(json!({"name": to}))).await {
                    Ok(_) => changed.run(()),
                    Err(e) => msg.err(e),
                }
            });
        });
    };
    let delete = move |id: String, name: String| {
        ask(
            Ask::new(format!("Delete the category “{name}”?"))
                .text("Its messages move to Uncategorized. No message is deleted.")
                .yes("Delete")
                .danger(),
            move |_| {
                spawn_local(async move {
                    match api("POST", &format!("{}/{id}/delete", base()), Some(json!({}))).await {
                        Ok(_) => changed.run(()),
                        Err(e) => msg.err(e),
                    }
                });
            },
        );
    };
    let drag_ok = !crate::channels::coarse_pointer();
    // Outside the redrawn part, so a screen reader hears each move.
    let list = move || {
        let needle = q.get().trim().to_lowercase();
        let all = rows.get();
        let cs = cats.get();
        let order = flat(&all, &cs);
        let n = order.len();
        let by_id = |id: &str| all.iter().find(|m| m["id"].as_i64().unwrap_or(0).to_string() == id).cloned();
        let hit = |m: &Value| needle.is_empty() || m["name"].as_str().unwrap_or("").to_lowercase().contains(&needle);
        let line = move |i: usize, m: Value| {
            view! {
                <div class="msort" data-i=i.to_string() draggable=if drag_ok { "true" } else { "false" }
                    class:moving=move || drag.get() == Some(i)
                    class:above=move || over.get() == Some(i) && drag.get().is_some_and(|d| d > i)
                    class:below=move || over.get() == Some(i) && drag.get().is_some_and(|d| d < i)
                    on:dragstart=move |e| { if let Some(dt) = e.data_transfer() { let _ = dt.set_data("text/plain", ""); } drag.set(Some(i)) }
                    on:dragover=move |e| { if drag.get_untracked().is_some() { e.prevent_default(); over.set(Some(i)) } }
                    on:dragend=move |_| { drag.set(None); over.set(None) }
                    on:drop=move |e| { e.prevent_default(); if let Some(d) = drag.get_untracked() { place((d, i)) } drag.set(None); over.set(None) }
                    on:keydown=move |e| {
                        if e.target() != e.current_target() { return; }
                        let down = match e.key().as_str() { "ArrowUp" => false, "ArrowDown" => true, _ => return };
                        e.prevent_default();
                        go(i, i, Some(down));
                    }
                    tabindex="0" aria-describedby="msort-hint">
                    <Grip i drag over drop=Callback::new(place) />
                    {row.run(m)}
                </div>
            }
        };
        let shown: Vec<(usize, Value)> = order.iter().enumerate().filter_map(|(i, (id, _))| by_id(id).filter(|m| hit(m)).map(|m| (i, m))).collect();
        if cs.is_empty() {
            // No categories yet: the plain list, still sortable.
            return view! { <div class="mlist msorted">{shown.into_iter().map(|(i, m)| line(i, m)).collect_view()}</div> }.into_any();
        }
        let searching = !needle.is_empty();
        let ncats = cs.len();
        let groups = cs.iter().enumerate().map(|(k, c)| (Some(k), c["id"].as_str().unwrap_or("").to_string(), c["name"].as_str().unwrap_or("").to_string()))
            .chain([(None, String::new(), "Uncategorized".to_string())]);
        let body = groups.filter_map(|(k, id, name)| {
            let items: Vec<(usize, Value)> = shown.iter().filter(|(i, _)| order[*i].1 == id).cloned().collect();
            // Uncategorized shows only when it holds messages; an empty own category shows (a drop target).
            if (k.is_none() || searching) && items.is_empty() {
                return None;
            }
            let key = if id.is_empty() { "-".to_string() } else { id.clone() };
            let key2 = key.clone();
            let key = StoredValue::new(key);
            let open = move || searching || key.with_value(|k| opened.with(|o| o.contains(k)));
            let count = items.len();
            let head_i = k.map(|k| n + k);
            let (id2, name2, id3, name3) = (id.clone(), name.clone(), id.clone(), name.clone());
            Some(view! {
                <div class="hubarea mcat">
                    <div class="mcathead" data-i=head_i.map(|h| h.to_string())
                        draggable=if drag_ok && head_i.is_some() { "true" } else { "false" }
                        class:moving=move || head_i.is_some() && drag.get() == head_i
                        class:target=move || head_i.is_some() && over.get() == head_i && drag.get().is_some_and(|d| d < n)
                        class:above=move || head_i.is_some() && over.get() == head_i && drag.get().is_some_and(|d| d >= n && Some(d) > head_i)
                        class:below=move || head_i.is_some() && over.get() == head_i && drag.get().is_some_and(|d| d >= n && Some(d) < head_i)
                        on:dragstart=move |e| if let Some(h) = head_i { if let Some(dt) = e.data_transfer() { let _ = dt.set_data("text/plain", ""); } drag.set(Some(h)) }
                        on:dragover=move |e| if let (Some(h), true) = (head_i, drag.get_untracked().is_some()) { e.prevent_default(); over.set(Some(h)) }
                        on:dragend=move |_| { drag.set(None); over.set(None) }
                        on:drop=move |e| { e.prevent_default(); if let (Some(d), Some(h)) = (drag.get_untracked(), head_i) { place((d, h)) } drag.set(None); over.set(None) }>
                        {head_i.map(|h| view! { <Grip i=h drag over drop=Callback::new(place) /> })}
                        <HubRow label=name n=count open=Signal::derive(open)
                            flip=Callback::new(move |_| opened.update(|o| if !o.remove(&key2) { o.insert(key2.clone()); }))
                            attr:aria-describedby=head_i.map(|_| "msort-hint")
                            on:keydown=move |e: web_sys::KeyboardEvent| if let Some(h) = head_i {
                                let to = match e.key().as_str() { "ArrowUp" if h > n => h - 1, "ArrowDown" if h + 1 < n + ncats => h + 1, _ => return };
                                e.prevent_default();
                                place((h, to));
                            } />
                    </div>
                    <Show when=open>
                        {k.map(|_| { let (a, b, c, d) = (id2.clone(), name2.clone(), id3.clone(), name3.clone()); view! { <div class="mcattools">
                            <button class="ghost" title="Rename category" aria-label="Rename category" on:click=move |_| rename(a.clone(), b.clone())>{crate::icons::icon("edit")}</button>
                            <button class="ghost" title="Delete category" aria-label="Delete category" on:click=move |_| delete(c.clone(), d.clone())>{crate::icons::icon("trash")}</button>
                        </div> } })}
                        <div class="mlist msorted">{items.clone().into_iter().map(|(i, m)| line(i, m)).collect_view()}</div>
                        {items.is_empty().then(|| view! { <p class="muted">"No messages here yet. Drag one here, or move one with the arrow keys."</p> })}
                    </Show>
                </div>
            })
        }).collect_view();
        view! { {body} }.into_any()
    };
    view! { <p class="sr" id="msort-hint">"Use the up and down arrow keys to move the focused message or category."</p><p class="sr" aria-live="polite">{move || said.get()}</p>{list} }
}

/// "+ Category" and "Sort A to Z" beside the search.
pub fn tools(guild: StoredValue<String>, rows: RwSignal<Vec<Value>>, cats: RwSignal<Vec<Value>>, changed: Callback<()>, msg: Msg) -> impl IntoView {
    let base = move || format!("/api/guilds/{}/message-categories", guild.get_value());
    let add = move |_| {
        ask(Ask::new("New category").text("1 to 50 characters. Drag messages onto it.").yes("Add").input(""), move |name| {
            spawn_local(async move {
                match api("POST", &base(), Some(json!({"name": name}))).await {
                    Ok(_) => changed.run(()),
                    Err(e) => msg.err(e),
                }
            });
        });
    };
    let reset = move |_| {
        spawn_local(async move {
            match api("POST", &format!("{}/order", base()), Some(json!({"reset": true}))).await {
                Ok(_) => changed.run(()),
                Err(e) => msg.err(e),
            }
        });
    };
    let manual = move || rows.with(|r| r.iter().any(|m| m["position"].as_i64().unwrap_or(0) > 0)) || cats.with(|c| c.iter().any(|x| x["position"].as_i64().unwrap_or(0) > 0));
    view! {
        <button class="ghost" disabled=move || cats.with(|c| c.len() >= 25) title="Up to 25 categories" on:click=add>"New category"</button>
        {move || manual().then(|| view! { <button class="ghost" on:click=reset>"Sort A to Z"</button> })}
    }
}

#[cfg(test)]
mod tests {
    use super::{moved, step};

    fn o(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn a_drop_moves_rows_and_categories() {
        let order = o(&[("1", "a"), ("2", "a"), ("3", "")]);
        let cats = vec!["a".to_string(), "b".to_string()];
        // Down onto an Uncategorized row: it lands there and leaves its category.
        let (r, _) = moved(order.clone(), cats.clone(), (0, 2)).unwrap();
        assert_eq!(r, o(&[("2", "a"), ("3", ""), ("1", "")]));
        // Down one inside its category: it stays there.
        let (r, _) = moved(order.clone(), cats.clone(), (0, 1)).unwrap();
        assert_eq!(r, o(&[("2", "a"), ("1", "a"), ("3", "")]));
        // Up onto the last row of a: it joins a.
        let (r, _) = moved(order.clone(), cats.clone(), (2, 1)).unwrap();
        assert_eq!(r, o(&[("1", "a"), ("3", "a"), ("2", "a")]));
        // Onto category b's title (place 3 + 1): it goes to the end of b.
        let (r, _) = moved(order.clone(), cats.clone(), (2, 4)).unwrap();
        assert_eq!(r.last().unwrap(), &("3".to_string(), "b".to_string()));
        // A title onto a title: the categories swap.
        let (_, c) = moved(order.clone(), cats.clone(), (4, 3)).unwrap();
        assert_eq!(c, ["b", "a"]);
        assert!(moved(order.clone(), cats.clone(), (3, 0)).is_none());
        assert!(moved(order, cats, (1, 1)).is_none());
    }

    #[test]
    fn arrow_keys_reach_every_category() {
        let order = o(&[("1", "a"), ("2", "a"), ("3", "")]);
        let cats = vec!["a".to_string(), "b".to_string()];
        // Inside a category: a swap.
        let (r, _) = step(order.clone(), cats.clone(), 0, true).unwrap();
        assert_eq!(r, o(&[("2", "a"), ("1", "a"), ("3", "")]));
        // Down past the end of a: into the empty b.
        let (r, _) = step(order.clone(), cats.clone(), 1, true).unwrap();
        assert_eq!(r, o(&[("1", "a"), ("2", "b"), ("3", "")]));
        // Up from the top of Uncategorized: into the empty b, then to the end of a.
        let (r, c) = step(order.clone(), cats.clone(), 2, false).unwrap();
        assert_eq!(r, o(&[("1", "a"), ("2", "a"), ("3", "b")]));
        let (r, _) = step(r, c, 2, false).unwrap();
        assert_eq!(r, o(&[("1", "a"), ("2", "a"), ("3", "a")]));
        // Down past the last category: Uncategorized; past that, nothing.
        assert!(step(order.clone(), cats.clone(), 2, true).is_none());
        assert!(step(order, cats, 0, false).is_none());
    }
}
