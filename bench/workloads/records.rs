// Same algorithm as records.nx.
//
// The mirror of NX's `self` and `mut self` is a `&self` and a `&mut self`
// pair over the same struct, so the phases that cost a clone in NX are free
// here. That is the row: it prices copy-on-bind rather than a difference
// between record layouts, since both sides store three i64s.
//
// Phase 4 is the only place the two are the same shape, and that is the
// point of including it -- it is the control for phases 1 and 3.

#[derive(PartialEq, Clone)]
struct Cell {
    v: i64,
    tag: i64,
}

impl Cell {
    fn get(&self) -> i64 {
        self.v
    }

    fn bumped(&mut self) {
        self.v += 1;
        self.tag += self.v;
    }

    fn retagged(&mut self, k: i64) {
        self.tag = self.tag * 3 + k;
    }

    fn made(base: i64) -> Cell {
        Cell { v: base, tag: 0 }
    }

    fn folded(&self) -> i64 {
        self.v * 2 + self.tag
    }
}

fn main() {
    let rounds: usize = 2000000;

    let mut acc: i64 = 0;
    let c = Cell::made(1);
    let mut r: usize = 0;
    while r < rounds {
        acc += c.get();
        r += 1;
    }
    println!("{} {} {}", acc, c.v, c.tag);

    let mut facc: i64 = 0;
    let mut r: usize = 0;
    while r < rounds {
        facc += c.folded();
        r += 1;
    }
    println!("{}", facc);

    let mut cells: Vec<Cell> = Vec::with_capacity(1000);
    let mut i: i64 = 0;
    while i < 1000 {
        cells.push(Cell { v: i, tag: i * 2 });
        i += 1;
    }

    // Phase 3 mirrors NX's `for cell in cells:` semantics exactly, which are
// not Rust's: an NX loop variable is a fresh copy per iteration, so
// `cell.bumped()` writes back into the loop variable and leaves the list
// untouched. `iter_mut()` would mutate the collection in place and produce
// a different answer, so the loop is written out longhand.
let mut tacc: i64 = 0;
    let mut pass: usize = 0;
    while pass < 20 {
        let mut i: usize = 0;
        while i < cells.len() {
            let mut cell = cells[i].clone();
            cell.bumped();
            tacc += cell.v;
            i += 1;
        }
        pass += 1;
    }
    println!("{} {} {}", tacc, cells[0].v, cells[999].v);

    let mut q = Cell { v: 7, tag: 9 };
    q.v = 99;
    let mut p = q.clone();
    p.v = 1234;
    println!(
        "{} {} {} {}",
        q.v,
        p.v,
        q == p,
        q == Cell { v: 99, tag: 9 }
    );

    let mut lod: Vec<Cell> = Vec::with_capacity(2000);
    let mut i: i64 = 0;
    while i < 2000 {
        lod.push(Cell { v: i, tag: i + 1 });
        i += 1;
    }

    let mut idx: std::collections::BTreeMap<i64, Cell> = std::collections::BTreeMap::new();
    let mut i: i64 = 0;
    while i < 2000 {
        idx.insert(i * 7, Cell { v: i, tag: i });
        i += 1;
    }

    let mut sacc: i64 = 0;
    for cell in lod.iter() {
        sacc += cell.folded();
    }
    println!("{} {}", sacc, idx.len());

    let mut dacc: i64 = 0;
    for (_k, v) in idx.iter() {
        dacc += v.v;
    }
    println!("{}", dacc);

    let mut chain = Cell::made(5);
    chain.bumped();
    chain.bumped();
    chain.bumped();
    println!("{} {}", chain.v, chain.tag);

    cells[0].retagged(2);
    println!("{}", cells[0].tag);
}