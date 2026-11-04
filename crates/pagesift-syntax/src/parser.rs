//! GLR 解析主循环（对应官方 `parser.c` 的 lex / shift / reduce / accept / advance）。

use crate::Limits;
use crate::lexer::Lexer;
use crate::scanner::{ScanLexer, Scanner};
use crate::stack::{ERROR_STATE, Stack};
use crate::table::{Action, Language, SYM_ERROR};
use crate::tree::{Arena, F_EXTRA, F_KEYWORD, Len, NONE};

pub const MAX_VERSION_COUNT: usize = 6;
pub const MAX_VERSION_COUNT_OVERFLOW: usize = 4;
pub const MAX_SUMMARY_DEPTH: u32 = 16;

pub struct Parser<'a> {
    pub lang: &'a Language,
    pub a: Arena,
    pub st: Stack,
    pub lexer: Lexer<'a>,
    pub finished: u32,
    pub accept_count: u32,
    pub limits: Limits,
    pub steps: u64,
    pub step_limit: u64,
    started: std::time::Instant,
    pub aborted: bool,
    pub live_after: usize,
    pub live_st_after: usize,
    pub scanner: Option<Box<dyn Scanner>>,
    ext_buf: Vec<u8>,
}

pub struct ReduceAction {
    pub symbol: u16,
    pub count: u16,
    pub dprec: i16,
    pub prod: u16,
}

impl<'a> Parser<'a> {
    pub fn new(lang: &'a Language, input: &'a [u8], limits: Limits) -> Self {
        // 预留容量只占虚拟地址，页面写到才算常驻内存；避免向量翻倍扩容时新旧两份同时在内存里。
        let guess = (input.len() / 2 + 1024).min(limits.max_nodes);
        let mut a = Arena::default();
        a.nodes.reserve_exact(guess);
        a.kids.reserve_exact(guess);
        let scanner = if lang.external_token_count > 0 {
            crate::scanner::create(&lang.name)
        } else {
            None
        };
        let no_scanner = lang.external_token_count > 0 && scanner.is_none();
        Parser {
            lang,
            a,
            st: Stack::new(),
            lexer: Lexer::new(input),
            finished: NONE,
            accept_count: 0,
            limits,
            steps: 0,
            step_limit: if limits.max_steps > 0 {
                limits.max_steps
            } else {
                200_000 + 400 * input.len() as u64
            },
            started: std::time::Instant::now(),
            aborted: no_scanner,
            live_after: 0,
            live_st_after: 0,
            scanner,
            ext_buf: Vec::new(),
        }
    }

    /// 超出任何一项资源上限就标记中止。
    pub fn over_budget(&mut self) -> bool {
        if self.steps > self.step_limit
            || self.a.nodes.len() > self.limits.max_nodes
            || self.st.nodes.len() > self.limits.max_nodes.saturating_mul(2)
            || self.a.ext_total > (64 << 20)
        {
            self.aborted = true;
        }
        if self.limits.max_millis > 0
            && self.steps.is_multiple_of(256)
            && self.started.elapsed().as_millis() as u64 > self.limits.max_millis
        {
            self.aborted = true;
        }
        self.aborted
    }

    /// 官方 `ts_parser__lex`。返回 NONE 表示非终结符 extra 之后不需要前瞻。
    fn lex(&mut self, version: usize, parse_state: u16) -> u32 {
        let lang = self.lang;
        let mut mode = lang.lex_modes[parse_state as usize];
        if mode == 0xFFFF {
            return NONE;
        }
        let start = self.st.position(version);
        let mut error_mode = parse_state == ERROR_STATE;
        let mut ext = lang.ext_state(parse_state);
        let last_ext = self.st.heads[version].ext;
        let (mut found_external, mut ext_idx) = (false, 0usize);
        let mut skipped_error = false;
        let (mut first_char, mut err_start, mut err_end) = (0u32, Len::default(), Len::default());
        self.lexer.reset(start);
        loop {
            let cur = self.lexer.pos;
            if ext != 0
                && let Some(sc) = self.scanner.as_mut()
            {
                self.lexer.start();
                sc.deserialize(self.a.ext_bytes(last_ext));
                let mut sl = ScanLexer::new(&mut self.lexer);
                let mut found = sc.scan(&mut sl, lang.ext_valid_row(ext));
                let res = sl.result as usize;
                self.lexer.finish();
                if found && res < lang.ext_symbol_map.len() {
                    self.ext_buf.clear();
                    sc.serialize(&mut self.ext_buf);
                    let changed = self.ext_buf.as_slice() != self.a.ext_bytes(last_ext);
                    let end = self.lexer.token_end.unwrap_or(self.lexer.pos);
                    if end.bytes <= cur.bytes && !changed {
                        let next = lang.next_state(parse_state, lang.ext_symbol_map[res]);
                        if error_mode
                            || !self.st.has_advanced_since_error(&self.a, version)
                            || next == parse_state
                        {
                            found = false;
                        }
                    }
                } else {
                    found = false;
                }
                if found {
                    found_external = true;
                    ext_idx = res;
                    break;
                }
                self.lexer.reset(cur);
            }
            self.lexer.start();
            let found = self.lexer.run(&lang.lex, mode);
            self.lexer.finish();
            if found {
                break;
            }
            if !error_mode {
                error_mode = true;
                mode = lang.lex_modes[ERROR_STATE as usize];
                ext = lang.ext_state(ERROR_STATE);
                self.lexer.reset(start);
                continue;
            }
            if !skipped_error {
                skipped_error = true;
                err_start = self.lexer.token_start;
                err_end = self.lexer.token_start;
                first_char = self.lexer.look();
            }
            if self.lexer.pos.bytes == err_end.bytes {
                if self.lexer.eof() {
                    self.lexer.result = SYM_ERROR;
                    break;
                }
                self.lexer.advance(false);
            }
            err_end = self.lexer.pos;
        }
        if skipped_error {
            let leaf = self.a.new_leaf(
                lang,
                SYM_ERROR,
                err_start.sub(start),
                err_end.sub(err_start),
            );
            self.a.n_mut(leaf).lchar = first_char;
            return leaf;
        }
        let mut symbol = self.lexer.result;
        let te = self.lexer.token_end.unwrap_or(self.lexer.pos);
        let ts = if te.bytes < self.lexer.token_start.bytes {
            te
        } else {
            self.lexer.token_start
        };
        let mut is_keyword = false;
        if found_external {
            symbol = lang.ext_symbol_map[ext_idx];
        } else if symbol == lang.keyword_capture_token && symbol != 0 {
            let end_byte = te.bytes;
            self.lexer.reset(ts);
            self.lexer.start();
            is_keyword = self.lexer.run(&lang.keyword_lex, 0);
            self.lexer.finish();
            let kend = self.lexer.token_end.unwrap_or(self.lexer.pos);
            if is_keyword
                && kend.bytes == end_byte
                && (lang.has_actions(parse_state, self.lexer.result)
                    || lang.is_reserved(parse_state, self.lexer.result))
            {
                symbol = self.lexer.result;
            }
        }
        let leaf = self.a.new_leaf(lang, symbol, ts.sub(start), te.sub(ts));
        if is_keyword {
            self.a.n_mut(leaf).flags |= F_KEYWORD;
        }
        if found_external {
            let idx = self.a.intern_ext(&self.ext_buf);
            self.a.n_mut(leaf).ext = idx;
        }
        leaf
    }

    pub fn shift(&mut self, version: usize, state: u16, lookahead: u32, extra: bool) {
        let mut t = lookahead;
        if extra != self.a.n(t).extra() && self.a.n(t).child_count == 0 {
            let mut c = self.a.n(t).clone();
            if extra {
                c.flags |= F_EXTRA
            } else {
                c.flags &= !F_EXTRA
            }
            self.a.nodes.push(c);
            t = (self.a.nodes.len() - 1) as u32;
        }
        self.st.push(&self.a, version, t, false, state);
    }

    fn remove_trailing_extras(&self, v: &mut Vec<u32>) -> Vec<u32> {
        let mut out = Vec::new();
        while let Some(&l) = v.last() {
            if self.a.n(l).extra() {
                out.push(l);
                v.pop();
            } else {
                break;
            }
        }
        out.reverse();
        out
    }

    /// 官方 `ts_parser__select_tree`：true 表示选右边。
    pub fn select_tree(&mut self, left: u32, right: u32) -> bool {
        if left == NONE {
            return true;
        }
        if right == NONE {
            return false;
        }
        let (l, r) = (self.a.n(left), self.a.n(right));
        if r.cost() < l.cost() {
            return true;
        }
        if l.cost() < r.cost() {
            return false;
        }
        if r.dprec > l.dprec {
            return true;
        }
        if l.dprec > r.dprec {
            return false;
        }
        if l.cost() > 0 {
            return true;
        }
        self.a.compare(left, right) > 0
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reduce(
        &mut self,
        version: usize,
        symbol: u16,
        count: u16,
        dprec: i16,
        prod: u16,
        is_fragile: bool,
        end_of_extra: bool,
    ) -> Option<usize> {
        let lang = self.lang;
        let initial = self.st.version_count();
        let pop = self.st.pop_count(&self.a, version, count as u32);
        let mut removed = 0usize;
        let halted = self.st.halted_count();
        let mut i = 0;
        let mut pop: Vec<Option<crate::stack::Slice>> = pop.into_iter().map(Some).collect();
        let n = pop.len();
        let _ = is_fragile;
        while i < n {
            let slice = pop[i].take().unwrap();
            let sv = slice.version - removed;
            if sv > MAX_VERSION_COUNT + MAX_VERSION_COUNT_OVERFLOW + halted {
                self.st.remove_version(sv);
                removed += 1;
                while i + 1 < n && pop[i + 1].as_ref().unwrap().version == slice.version {
                    i += 1;
                }
                i += 1;
                continue;
            }
            let mut children = slice.trees;
            let mut trailing = self.remove_trailing_extras(&mut children);
            let mut parent = self.a.new_node(lang, symbol, &children, prod);
            while i + 1 < n && pop[i + 1].as_ref().unwrap().version == slice.version {
                i += 1;
                let mut next = pop[i].take().unwrap().trees;
                let trailing2 = self.remove_trailing_extras(&mut next);
                // 官方 select_children：用临时节点比较，再丢掉
                let (nl, kl) = (self.a.nodes.len(), self.a.kids.len());
                let scratch = self.a.new_node(lang, self.a.n(parent).sym, &next, 0);
                let take_new = self.select_tree(parent, scratch);
                self.a.nodes.truncate(nl);
                self.a.kids.truncate(kl);
                if take_new {
                    trailing = trailing2;
                    parent = self.a.new_node(lang, symbol, &next, prod);
                }
            }
            let state = self.st.state(sv);
            let next_state = lang.next_state(state, symbol);
            if end_of_extra && next_state == state {
                self.a.n_mut(parent).flags |= F_EXTRA;
            }
            self.a.n_mut(parent).dprec += dprec as i32;
            self.st.push(&self.a, sv, parent, false, next_state);
            for &t in &trailing {
                self.st.push(&self.a, sv, t, false, next_state);
            }
            for j in 0..sv {
                if j == version {
                    continue;
                }
                if self.st.merge(&self.a, j, sv) {
                    removed += 1;
                    break;
                }
            }
            i += 1;
        }
        if self.st.version_count() > initial {
            Some(initial)
        } else {
            None
        }
    }

    pub fn accept(&mut self, version: usize, lookahead: u32) {
        let lang = self.lang;
        self.st.push(&self.a, version, lookahead, false, 1);
        let pop = self.st.pop_all(&self.a, version);
        let first_version = pop.first().map(|s| s.version);
        for slice in pop {
            let mut trees = slice.trees;
            let mut root = NONE;
            for j in (0..trees.len()).rev() {
                let t = trees[j];
                if !self.a.n(t).extra() {
                    let ch: Vec<u32> = self.a.children(t).to_vec();
                    trees.splice(j..j + 1, ch);
                    let (sym, prod) = (self.a.n(t).sym, self.a.n(t).prod);
                    root = self.a.new_node(lang, sym, &trees, prod);
                    break;
                }
            }
            if root == NONE {
                continue;
            }
            self.accept_count += 1;
            if self.finished != NONE {
                if self.select_tree(self.finished, root) {
                    self.finished = root;
                }
            } else {
                self.finished = root;
            }
        }
        if let Some(v) = first_version {
            self.st.remove_version(v);
        }
        self.st.halt(version);
    }

    /// 官方 `ts_parser__advance`：返回 false 表示超出资源上限。
    pub fn advance(&mut self, version: usize) -> bool {
        let lang = self.lang;
        let mut state = self.st.state(version);
        let mut lookahead = NONE;
        let mut actions: &'a [Action] = &[];
        let mut needs_lex = true;
        loop {
            if needs_lex {
                needs_lex = false;
                lookahead = self.lex(version, state);
                actions = if lookahead != NONE {
                    lang.actions_for(state, self.a.n(lookahead).sym)
                } else {
                    lang.actions_for(state, 0)
                };
            }
            self.steps += 1;
            if self.over_budget()
                || self.st.nodes[self.st.heads[version].node as usize].depth as usize
                    > self.limits.max_stack_depth
            {
                self.aborted = true;
                return false;
            }
            let leaf_sym = if lookahead != NONE {
                self.a.n(lookahead).sym
            } else {
                0
            };
            let mut did_reduce = false;
            let mut last_reduction: Option<usize> = None;
            for act in actions {
                match *act {
                    Action::Shift {
                        state: s,
                        extra,
                        repetition,
                    } => {
                        if repetition {
                            continue;
                        }
                        let next = if extra { state } else { s };
                        self.shift(version, next, lookahead, extra);
                        return true;
                    }
                    Action::Reduce {
                        symbol,
                        count,
                        dprec,
                        prod,
                    } => {
                        let fragile = actions.len() > 1;
                        let eoe = lookahead == NONE;
                        if let Some(rv) =
                            self.reduce(version, symbol, count, dprec, prod, fragile, eoe)
                        {
                            last_reduction = Some(rv);
                        }
                        did_reduce = true;
                    }
                    Action::Accept => {
                        self.accept(version, lookahead);
                        return true;
                    }
                    Action::Recover => {
                        self.recover(version, lookahead);
                        return true;
                    }
                    Action::Header(_) => {}
                }
            }
            if let Some(rv) = last_reduction {
                self.st.renumber_version(rv, version);
                state = self.st.state(version);
                if lookahead == NONE {
                    needs_lex = true;
                } else {
                    actions = lang.actions_for(state, leaf_sym);
                }
                continue;
            }
            if did_reduce {
                self.st.halt(version);
                return true;
            }
            if lookahead != NONE {
                let n = self.a.n(lookahead);
                if n.flags & F_KEYWORD != 0
                    && n.sym != lang.keyword_capture_token
                    && !lang.is_reserved(state, n.sym)
                {
                    let alt = lang.actions_for(state, lang.keyword_capture_token);
                    if !alt.is_empty() {
                        let mut c = n.clone();
                        c.sym = lang.keyword_capture_token;
                        c.flags = (c.flags & !(crate::tree::F_VISIBLE | crate::tree::F_NAMED))
                            | if lang.visible(c.sym) {
                                crate::tree::F_VISIBLE
                            } else {
                                0
                            }
                            | if lang.named(c.sym) {
                                crate::tree::F_NAMED
                            } else {
                                0
                            };
                        self.a.nodes.push(c);
                        lookahead = (self.a.nodes.len() - 1) as u32;
                        actions = alt;
                        continue;
                    }
                }
            }
            if state == ERROR_STATE {
                self.recover(version, lookahead);
                return true;
            }
            self.st.pause(version, lookahead);
            return true;
        }
    }

    /// 主循环。成功后根节点在 `self.finished`（可能是 NONE）。
    pub fn run(&mut self) {
        let (mut position, mut last_position);
        last_position = 0u32;
        loop {
            let mut version = 0usize;
            let version_count;
            loop {
                let vc = self.st.version_count();
                if version >= vc {
                    version_count = vc;
                    break;
                }
                while self.st.is_active(version) {
                    if !self.advance(version) {
                        return;
                    }
                    position = self.st.position(version).bytes;
                    if position > last_position || (version > 0 && position == last_position) {
                        last_position = position;
                        break;
                    }
                }
                version += 1;
            }
            let min_cost = self.condense_stack();
            if self.aborted {
                return;
            }
            self.maybe_compact();
            if self.finished != NONE && self.a.n(self.finished).cost() < min_cost {
                self.st.clear();
                break;
            }
            if version_count == 0 {
                break;
            }
        }
    }
}
