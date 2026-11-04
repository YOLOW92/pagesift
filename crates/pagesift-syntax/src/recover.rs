//! 错误恢复与版本裁剪（对应官方 `parser.c` 中的 recover / handle_error / condense_stack）。

use crate::parser::{MAX_SUMMARY_DEPTH, MAX_VERSION_COUNT, Parser, ReduceAction};
use crate::stack::ERROR_STATE;
use crate::table::{Action, SYM_ERROR_REPEAT};
use crate::tree::*;

const MAX_COST_DIFFERENCE: u32 = 18 * ERROR_COST_PER_SKIPPED_TREE;

#[derive(Clone, Copy)]
pub struct ErrorStatus {
    cost: u32,
    node_count: u32,
    dprec: i32,
    in_error: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Cmp {
    None,
    PreferLeft,
    PreferRight,
    TakeLeft,
    TakeRight,
}

fn compare_versions(a: ErrorStatus, b: ErrorStatus) -> Cmp {
    if !a.in_error && b.in_error {
        return if a.cost < b.cost {
            Cmp::TakeLeft
        } else {
            Cmp::PreferLeft
        };
    }
    if a.in_error && !b.in_error {
        return if b.cost < a.cost {
            Cmp::TakeRight
        } else {
            Cmp::PreferRight
        };
    }
    if a.cost < b.cost {
        return if (b.cost - a.cost).wrapping_mul(1 + a.node_count) > MAX_COST_DIFFERENCE {
            Cmp::TakeLeft
        } else {
            Cmp::PreferLeft
        };
    }
    if b.cost < a.cost {
        return if (a.cost - b.cost).wrapping_mul(1 + b.node_count) > MAX_COST_DIFFERENCE {
            Cmp::TakeRight
        } else {
            Cmp::PreferRight
        };
    }
    if a.dprec > b.dprec {
        return Cmp::PreferLeft;
    }
    if b.dprec > a.dprec {
        return Cmp::PreferRight;
    }
    Cmp::None
}

impl Parser<'_> {
    fn version_status(&mut self, v: usize) -> ErrorStatus {
        let mut cost = self.st.error_cost(v);
        let paused = self.st.is_paused(v);
        if paused {
            cost += ERROR_COST_PER_SKIPPED_TREE;
        }
        ErrorStatus {
            cost,
            node_count: self.st.node_count_since_error(v),
            dprec: self.st.dynamic_precedence(v),
            in_error: paused || self.st.state(v) == ERROR_STATE,
        }
    }

    fn better_version_exists(&mut self, version: usize, in_error: bool, cost: u32) -> bool {
        if self.finished != NONE && self.a.n(self.finished).cost() <= cost {
            return true;
        }
        let pos = self.st.position(version);
        let status = ErrorStatus {
            cost,
            in_error,
            dprec: self.st.dynamic_precedence(version),
            node_count: self.st.node_count_since_error(version),
        };
        for i in 0..self.st.version_count() {
            if i == version || !self.st.is_active(i) || self.st.position(i).bytes < pos.bytes {
                continue;
            }
            let si = self.version_status(i);
            match compare_versions(status, si) {
                Cmp::TakeRight => return true,
                Cmp::PreferRight if self.st.can_merge(i, version) => {
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    fn process_candidates(acts: &[Action], out: &mut Vec<ReduceAction>) -> bool {
        let mut has_shift = false;
        for a in acts {
            match *a {
                Action::Shift {
                    extra, repetition, ..
                } => {
                    if !extra && !repetition {
                        has_shift = true;
                    }
                }
                Action::Recover => has_shift = true,
                Action::Reduce {
                    symbol,
                    count,
                    dprec,
                    prod,
                } if count > 0 && !out.iter().any(|r| r.symbol == symbol && r.count == count) => {
                    out.push(ReduceAction {
                        symbol,
                        count,
                        dprec,
                        prod,
                    });
                }
                _ => {}
            }
        }
        has_shift
    }

    fn do_all_potential_reductions(&mut self, starting: usize, lookahead_symbol: u16) -> bool {
        let lang = self.lang;
        let initial = self.st.version_count();
        let mut can_shift = false;
        let mut version = starting;
        let mut i = 0usize;
        loop {
            let ci = i;
            i += 1;
            let vc = self.st.version_count();
            if version >= vc {
                break;
            }
            let mut merged = false;
            for j in initial..version {
                if self.st.merge(&self.a, j, version) {
                    merged = true;
                    break;
                }
            }
            if merged {
                continue;
            }
            let state = self.st.state(version);
            let mut reds: Vec<ReduceAction> = Vec::new();
            let mut has_shift = false;
            if lookahead_symbol != 0 {
                has_shift =
                    Self::process_candidates(lang.actions_for(state, lookahead_symbol), &mut reds);
            } else {
                let (s, e) = (
                    lang.state_start[state as usize] as usize,
                    lang.state_start[state as usize + 1] as usize,
                );
                for &ent in &lang.entries[s..e] {
                    let sym = (ent >> 16) as u16;
                    if sym == 0 || sym as u32 >= lang.token_count {
                        continue;
                    }
                    if Self::process_candidates(lang.actions_for(state, sym), &mut reds) {
                        has_shift = true;
                    }
                }
                // 稳定的降序插入排序
                for j in 1..reds.len() {
                    let mut k = j;
                    while k > 0 && reds[k - 1].symbol < reds[k].symbol {
                        reds.swap(k - 1, k);
                        k -= 1;
                    }
                }
            }
            let mut reduction: Option<usize> = None;
            for r in &reds {
                reduction = self.reduce(version, r.symbol, r.count, r.dprec, r.prod, true, false);
            }
            if has_shift {
                can_shift = true;
            } else if reduction.is_some() && ci < MAX_VERSION_COUNT {
                self.st.renumber_version(reduction.unwrap(), version);
                continue;
            } else if lookahead_symbol != 0 {
                self.st.remove_version(version);
                continue;
            }
            version = if version == starting { vc } else { version + 1 };
            if self.over_budget() {
                break;
            }
        }
        can_shift
    }

    fn recover_to_state(&mut self, version: usize, depth: u32, goal: u16) -> bool {
        let lang = self.lang;
        let pop = self.st.pop_count(&self.a, version, depth);
        let mut previous: Option<usize> = None;
        for slice in pop {
            if Some(slice.version) == previous {
                continue;
            }
            if self.st.state(slice.version) != goal {
                self.st.halt(slice.version);
                continue;
            }
            let mut trees = slice.trees;
            let err = self.st.pop_error(&self.a, slice.version);
            if let Some(&et) = err.first() {
                let kids: Vec<u32> = self.a.children(et).to_vec();
                if !kids.is_empty() {
                    let nested = self.a.new_node(lang, SYM_ERROR_REPEAT, &kids, 0);
                    trees.insert(0, nested);
                }
            }
            let mut trailing = Vec::new();
            while let Some(&l) = trees.last() {
                if self.a.n(l).extra() {
                    trailing.push(l);
                    trees.pop();
                } else {
                    break;
                }
            }
            trailing.reverse();
            if !trees.is_empty() {
                let e = self.a.new_error_node(lang, &trees, true);
                self.st.push(&self.a, slice.version, e, false, goal);
            }
            for t in trailing {
                self.st.push(&self.a, slice.version, t, false, goal);
            }
            previous = Some(slice.version);
        }
        previous.is_some()
    }

    pub fn recover(&mut self, version: usize, lookahead: u32) {
        let lang = self.lang;
        let mut did_recover = false;
        let prev_count = self.st.version_count();
        let position = self.st.position(version);
        let summary = self.st.heads[version].summary.clone();
        let nc_since = self.st.node_count_since_error(version);
        let cur_cost = self.st.error_cost(version);
        let la_sym = if lookahead == NONE {
            0
        } else {
            self.a.n(lookahead).sym
        };
        let la_is_error = lookahead != NONE && self.a.n(lookahead).is_error();
        if let Some(sum) = summary.filter(|_| !la_is_error) {
            for entry in sum {
                if entry.state == ERROR_STATE || entry.pos.bytes == position.bytes {
                    continue;
                }
                let mut depth = entry.depth;
                if nc_since > 0 {
                    depth += 1;
                }
                let would_merge = (0..prev_count).any(|j| {
                    self.st.state(j) == entry.state && self.st.position(j).bytes == position.bytes
                });
                if would_merge {
                    continue;
                }
                let new_cost = cur_cost
                    .wrapping_add(entry.depth * ERROR_COST_PER_SKIPPED_TREE)
                    .wrapping_add((position.bytes - entry.pos.bytes) * ERROR_COST_PER_SKIPPED_CHAR)
                    .wrapping_add((position.rows - entry.pos.rows) * ERROR_COST_PER_SKIPPED_LINE);
                if self.better_version_exists(version, false, new_cost) {
                    break;
                }
                if lang.has_actions(entry.state, la_sym)
                    && self.recover_to_state(version, depth, entry.state)
                {
                    did_recover = true;
                    break;
                }
            }
        }
        let mut i = prev_count;
        while i < self.st.version_count() {
            if !self.st.is_active(i) {
                self.st.remove_version(i);
            } else {
                i += 1;
            }
        }
        if la_sym == 0 && lookahead != NONE {
            let parent = self.a.new_error_node(lang, &[], false);
            self.st.push(&self.a, version, parent, false, 1);
            self.accept(version, lookahead);
            return;
        }
        if did_recover && self.st.version_count() > MAX_VERSION_COUNT {
            self.st.halt(version);
            return;
        }
        let la = self.a.n(lookahead);
        let new_cost = cur_cost
            .wrapping_add(ERROR_COST_PER_SKIPPED_TREE)
            .wrapping_add(la.total().bytes * ERROR_COST_PER_SKIPPED_CHAR)
            .wrapping_add(la.total().rows * ERROR_COST_PER_SKIPPED_LINE);
        if self.better_version_exists(version, false, new_cost) {
            self.st.halt(version);
            return;
        }
        let mut lookahead = lookahead;
        let acts = lang.actions_for(1, la_sym);
        if let Some(Action::Shift { extra: true, .. }) = acts.last() {
            let mut c = self.a.n(lookahead).clone();
            c.flags |= F_EXTRA;
            self.a.nodes.push(c);
            lookahead = (self.a.nodes.len() - 1) as u32;
        }
        let mut error_repeat = self.a.new_node(lang, SYM_ERROR_REPEAT, &[lookahead], 0);
        if nc_since > 0 {
            let mut pop = self.st.pop_count(&self.a, version, 1);
            if pop.is_empty() {
                return;
            }
            let first_v = pop[0].version;
            if pop.len() > 1 {
                while self.st.version_count() > first_v + 1 {
                    self.st.remove_version(first_v + 1);
                }
            }
            self.st.renumber_version(first_v, version);
            let mut kids = std::mem::take(&mut pop[0].trees);
            kids.push(error_repeat);
            error_repeat = self.a.new_node(lang, SYM_ERROR_REPEAT, &kids, 0);
        }
        self.st
            .push(&self.a, version, error_repeat, false, ERROR_STATE);
        let mut has_error = true;
        for i in 0..self.st.version_count() {
            if !self.version_status(i).in_error {
                has_error = false;
                break;
            }
        }
        let _ = has_error;
    }

    pub fn handle_error(&mut self, version: usize, lookahead: u32) {
        let lang = self.lang;
        let prev_count = self.st.version_count();
        self.do_all_potential_reductions(version, 0);
        let version_count = self.st.version_count();
        let la_sym = if lookahead == NONE {
            0
        } else {
            self.a.n(lookahead).sym
        };
        let mut did_insert = false;
        let mut v = version;
        while v < version_count {
            if !did_insert {
                let state = self.st.state(v);
                for missing in 1..(lang.token_count as u16) {
                    let after = lang.next_state(state, missing);
                    if after == 0 || after == state {
                        continue;
                    }
                    if lang.has_reduce_action(after, la_sym) {
                        let vm = self.st.copy_version(v);
                        let mt = self.a.new_missing_leaf(lang, missing);
                        self.st.push(&self.a, vm, mt, false, after);
                        if self.do_all_potential_reductions(vm, la_sym) {
                            did_insert = true;
                            break;
                        }
                    }
                }
            }
            self.st.push(&self.a, v, NONE, false, ERROR_STATE);
            v = if v == version { prev_count } else { v + 1 };
            if self.over_budget() {
                return;
            }
        }
        for _ in prev_count..version_count {
            if prev_count < self.st.version_count() {
                self.st.merge(&self.a, version, prev_count);
            }
        }
        self.st.record_summary(&self.a, version, MAX_SUMMARY_DEPTH);
        self.recover(version, lookahead);
    }

    pub fn condense_stack(&mut self) -> u32 {
        let mut min_cost = u32::MAX;
        let mut i: isize = 0;
        while (i as usize) < self.st.version_count() {
            if self.st.is_halted(i as usize) {
                self.st.remove_version(i as usize);
                continue;
            }
            let si = self.version_status(i as usize);
            if !si.in_error && si.cost < min_cost {
                min_cost = si.cost;
            }
            let mut j: isize = 0;
            while j < i {
                let sj = self.version_status(j as usize);
                match compare_versions(sj, si) {
                    Cmp::TakeLeft => {
                        self.st.remove_version(i as usize);
                        i -= 1;
                        j = i;
                    }
                    Cmp::PreferLeft | Cmp::None => {
                        if self.st.merge(&self.a, j as usize, i as usize) {
                            i -= 1;
                            j = i;
                        }
                    }
                    Cmp::PreferRight => {
                        if self.st.merge(&self.a, j as usize, i as usize) {
                            i -= 1;
                            j = i;
                        } else {
                            self.st.swap_versions(i as usize, j as usize);
                        }
                    }
                    Cmp::TakeRight => {
                        self.st.remove_version(j as usize);
                        i -= 1;
                        j -= 1;
                    }
                }
                j += 1;
            }
            i += 1;
        }
        while self.st.version_count() > MAX_VERSION_COUNT {
            self.st.remove_version(MAX_VERSION_COUNT);
        }
        if self.st.version_count() > 0 {
            let mut has_unpaused = false;
            let mut i = 0usize;
            while i < self.st.version_count() {
                if self.st.is_paused(i) {
                    if !has_unpaused && (self.accept_count as usize) < MAX_VERSION_COUNT {
                        min_cost = self.st.error_cost(i);
                        let la = self.st.resume(i);
                        self.handle_error(i, la);
                        has_unpaused = true;
                    } else {
                        self.st.remove_version(i);
                        continue;
                    }
                } else {
                    has_unpaused = true;
                }
                i += 1;
            }
        }
        min_cost
    }
}
