//! Slots and value plumbing: typed locals and raw/boxed conversions.

use crate::core::Gen;
use crate::value::{ll_scalar, NV};
use nx_types::Ty;

impl Gen {
    // --- unboxing -----------------------------------------------------

    /// Static type of `name` in the function being emitted, if known.
    /// Unknowable when unboxing is off, which is what makes the opt-out
    /// reproduce the old all-boxed code exactly.
    pub(crate) fn ty_of(&self, name: &str) -> Ty {
        if !self.unbox_on {
            return Ty::Unknown;
        }
        self.types
            .get(&(self.cur_module.clone(), self.cur_fn.clone()))
            .and_then(|f| f.locals.get(name))
            .cloned()
            .unwrap_or(Ty::Unknown)
    }

    /// Static type of `name` in the function being emitted, ignoring the
    /// unboxing opt-out. Method dispatch needs this: `self`'s type comes
    /// from the receiver rather than from inference, so it is known even in
    /// the all-boxed build. Unboxing is a representation choice, not a
    /// typing one -- a call that resolves in one build must resolve in the
    /// other, or NX_NOUNBOX stops being a debug switch and becomes a
    /// different language.
    pub(crate) fn ty_dispatch(&self, name: &str) -> Ty {
        self.types
            .get(&(self.cur_module.clone(), self.cur_fn.clone()))
            .and_then(|f| f.locals.get(name))
            .cloned()
            .unwrap_or(Ty::Unknown)
    }

    /// Static type of `name` wherever it lives: function locals first,
    /// then module globals. `ty_of` alone misses globals (it keys on the
    /// current function), which is what sent every top-level `push` down
    /// the dynamic path.
    pub(crate) fn ty_of_any(&self, name: &str) -> Ty {
        if self.locals.contains_key(name) {
            return self.ty_of(name);
        }
        self.global_ty(name)
    }

    /// Representation chosen for `name`: Some(t) means the slot holds a
    /// bare `ll_scalar(t)`, None means it holds a boxed `%NxVal`.
    pub(crate) fn rep_of(&self, name: &str) -> Option<Ty> {
        self.rep.get(name).cloned()
    }

    /// Declare a local slot for `name` and record its representation.
    /// `hint` is the static type to unbox into, if any.
    pub(crate) fn new_slot(&mut self, name: &str, hint: Option<Ty>) -> String {
        match hint.as_ref().and_then(ll_scalar) {
            Some(ll) => {
                let slot = self.alloca(ll);
                self.rep.insert(name.to_string(), hint.unwrap());
                self.locals.insert(name.to_string(), slot.clone());
                slot
            }
            None => {
                let slot = self.alloca("%NxVal");
                self.w(&format!("  store %NxVal zeroinitializer, ptr {slot}"));
                self.rep.remove(name);
                self.locals.insert(name.to_string(), slot.clone());
                slot
            }
        }
    }

    /// Load a local as a value of its slot type.
    pub(crate) fn load_slot(&mut self, name: &str) -> NV {
        let slot = match self.locals.get(name).cloned() {
            Some(s) => s,
            None => return NV::dyn_boxed("zeroinitializer".to_string()),
        };
        match self.rep_of(name) {
            Some(t) => {
                let ll = ll_scalar(&t).unwrap();
                let v = self.reg();
                self.w(&format!("  {v} = load {ll}, ptr {slot}"));
                NV::raw(t, v)
            }
            None => {
                let v = self.reg();
                self.w(&format!("  {v} = load %NxVal, ptr {slot}"));
                NV::boxed_known(v, self.ty_of(name))
            }
        }
    }

    /// Whether storing a value of this static type must duplicate container
    /// storage. NX has value semantics for containers: `ys = xs` leaves
    /// `ys` independent, and a function argument never aliases the
    /// caller's value. Scalars need no copy, and strings are never mutated
    /// in place, so sharing one is observably identical to copying it.
    /// Everything else goes through `nx_clone`, which passes non-container
    /// tags through unchanged at runtime.
    pub(crate) fn needs_clone(ty: &Ty) -> bool {
        matches!(ty, Ty::List(_) | Ty::Dict(_) | Ty::Record(_) | Ty::Unknown)
    }

    /// Box a value for storage, cloning container storage so the stored
    /// binding owns its value outright. See `needs_clone`. A fresh value
    /// (newly allocated by this expression) skips the clone: there is no
    /// other owner to separate from, which is what keeps `s = s + a*b`
    /// from copying on every iteration.
    pub(crate) fn store_boxed(&mut self, v: &NV) -> String {
        let b = self.unbox(v);
        if v.fresh || !Self::needs_clone(&v.ty) {
            return b;
        }
        let c = self.reg();
        self.w(&format!("  {c} = call %NxVal @nx_clone(%NxVal {b})"));
        c
    }

    /// Store a value into a local that already owns this storage: the
    /// write-back path after an in-place container update. Unlike
    /// `store_slot`, this never clones -- the value derives from the very
    /// binding it is stored into.
    pub(crate) fn store_slot_owned(&mut self, name: &str, v: &NV) {
        let slot = match self.locals.get(name).cloned() {
            Some(s) => s,
            None => return,
        };
        match self.rep_of(name) {
            Some(t) => {
                let ll = ll_scalar(&t).unwrap();
                let val = self.coerce(v, &t);
                self.w(&format!("  store {ll} {val}, ptr {slot}"));
            }
            None => {
                let val = self.unbox(v);
                self.w(&format!("  store %NxVal {val}, ptr {slot}"));
            }
        }
    }

    /// Store a value into a local, boxing if the slot is dynamic.
    pub(crate) fn store_slot(&mut self, name: &str, v: &NV) {
        let slot = match self.locals.get(name).cloned() {
            Some(s) => s,
            None => return,
        };
        match self.rep_of(name) {
            Some(t) => {
                let ll = ll_scalar(&t).unwrap();
                let val = self.coerce(v, &t);
                self.w(&format!("  store {ll} {val}, ptr {slot}"));
            }
            None => {
                let val = self.store_boxed(v);
                self.w(&format!("  store %NxVal {val}, ptr {slot}"));
            }
        }
    }

    /// Force a value into a boxed `%NxVal`, which is what every dynamic
    /// boundary (call argument, return, list element, global, print)
    /// takes. Returns the incoming register when it is already boxed.
    pub(crate) fn unbox(&mut self, v: &NV) -> String {
        let t = match v.raw.clone() {
            None => return v.reg.clone(),
            Some(t) => t,
        };
        let r = self.reg();
        let call = match t {
            Ty::Int => format!("@nx_int(i64 {})", v.reg),
            Ty::Float => format!("@nx_float(double {})", v.reg),
            Ty::Bool => format!("@nx_bool(i1 {})", v.reg),
            _ => unreachable!("only scalars are held raw"),
        };
        self.w(&format!("  {r} = call %NxVal {call}"));
        r
    }

    /// Payload of a value as a bare `i64`: field 1 of the box, or the
    /// register itself when it is already an integer. Float payloads come
    /// back as raw bits, so callers bitcast when they want a double.
    pub(crate) fn payload(&mut self, v: &NV) -> String {
        match v.raw {
            Some(Ty::Float) => {
                let r = self.reg();
                self.w(&format!("  {r} = bitcast double {} to i64", v.reg));
                r
            }
            Some(Ty::Int) => v.reg.clone(),
            _ => {
                let r = self.reg();
                self.w(&format!("  {r} = extractvalue %NxVal {}, 1", v.reg));
                r
            }
        }
    }

    /// Force a value into an `i1` branch condition.
    pub(crate) fn as_i1(&mut self, v: &NV) -> String {
        match v.raw {
            Some(Ty::Bool) => v.reg.clone(),
            Some(Ty::Int) => {
                let c = self.reg();
                self.w(&format!("  {c} = icmp ne i64 {}, 0", v.reg));
                c
            }
            Some(Ty::Float) => {
                let c = self.reg();
                self.w(&format!("  {c} = fcmp une double {}, 0.0", v.reg));
                c
            }
            _ => {
                // Untyped condition: keep the payload-is-nonzero rule the
                // boxed path has always used (the checker rejects non-Bool
                // conditions in any program that reaches the backend).
                let b = self.unbox(v);
                let r = self.reg();
                self.w(&format!("  {r} = extractvalue %NxVal {b}, 1"));
                let c = self.reg();
                self.w(&format!("  {c} = trunc i64 {r} to i1"));
                c
            }
        }
    }

    /// Force an integer operand to a bare `i64`.
    pub(crate) fn as_i64(&mut self, v: &NV) -> String {
        match v.raw {
            Some(Ty::Int) => v.reg.clone(),
            Some(Ty::Float) => {
                let r = self.reg();
                self.w(&format!("  {r} = fptosi double {} to i64", v.reg));
                r
            }
            _ => {
                let b = self.unbox(v);
                let r = self.reg();
                self.w(&format!("  {r} = extractvalue %NxVal {b}, 1"));
                r
            }
        }
    }

    /// Reinterpret a value as a `double`, unboxing it if needed.
    pub(crate) fn as_f64(&mut self, v: &NV) -> String {
        match v.raw {
            Some(Ty::Float) => v.reg.clone(),
            Some(Ty::Int) => {
                let r = self.reg();
                self.w(&format!("  {r} = sitofp i64 {} to double", v.reg));
                r
            }
            _ => {
                let b = self.unbox(v);
                let p = self.reg();
                self.w(&format!("  {p} = extractvalue %NxVal {b}, 1"));
                let r = self.reg();
                self.w(&format!("  {r} = bitcast i64 {p} to double"));
                r
            }
        }
    }

    /// Read a value as a scalar of its static type in a typed register,
    /// pulling the payload straight out of a box when needed. This is the
    /// bridge that lets a boxed global feed unboxed arithmetic. Returns
    /// None when unboxing is off or the type is unknown, so the caller
    /// takes the boxed path.
    pub(crate) fn as_raw(&mut self, v: &NV) -> Option<NV> {
        if !self.unbox_on {
            return None;
        }
        match v.raw {
            Some(_) => Some(v.clone()),
            None => {
                let t = v.ty.clone();
                let reg = match &t {
                    Ty::Int => self.payload(v),
                    Ty::Float => self.as_f64(v),
                    Ty::Bool => {
                        let p = self.payload(v);
                        let c = self.reg();
                        self.w(&format!("  {c} = trunc i64 {p} to i1"));
                        c
                    }
                    _ => return None,
                };
                // Unboxing preserves literal-ness: a boxed constant is
                // still a constant once its payload is pulled out.
                match v.const_i {
                    Some(k) => Some(NV::raw_const(t, reg, k)),
                    None => Some(NV::raw(t, reg)),
                }
            }
        }
    }

    /// The compile-time value of a register, when it is a literal. Used
    /// where a value has to be validated before emitting a raw
    /// instruction rather than deferred to a runtime helper.
    pub(crate) fn const_int(r: &NV) -> Option<i64> {
        r.const_i
    }

    /// Coerce a value to a target scalar type, inserting the numeric
    /// conversion the runtime's mixed Int/Float operators would apply.
    pub(crate) fn coerce(&mut self, v: &NV, want: &Ty) -> String {
        let src = match self.as_raw(v) {
            Some(s) => s,
            // Untyped value into a typed slot. The checker widens the slot
            // to Unknown when this can happen, so reaching here would mean
            // inference and codegen disagree; take the payload anyway
            // rather than emit a mismatched store.
            None => {
                if ll_scalar(want).is_none() {
                    return self.unbox(v);
                }
                let p = self.payload(v);
                return match want {
                    Ty::Float => {
                        let r = self.reg();
                        self.w(&format!("  {r} = bitcast i64 {p} to double"));
                        r
                    }
                    Ty::Bool => {
                        let r = self.reg();
                        self.w(&format!("  {r} = trunc i64 {p} to i1"));
                        r
                    }
                    _ => p,
                };
            }
        };
        if src.ty == *want {
            return src.reg;
        }
        let r = self.reg();
        match want {
            Ty::Float => self.w(&format!("  {r} = sitofp i64 {} to double", src.reg)),
            Ty::Int => self.w(&format!("  {r} = fptosi double {} to i64", src.reg)),
            _ => self.w(&format!("  {r} = trunc i64 {} to i1", src.reg)),
        }
        r
    }

    /// Static type of a module-level variable (always stored boxed).
    pub(crate) fn global_ty(&self, name: &str) -> Ty {
        self.types
            .get(&(self.cur_module.clone(), "<top>".to_string()))
            .and_then(|f| f.locals.get(name))
            .cloned()
            .unwrap_or(Ty::Unknown)
    }
}
