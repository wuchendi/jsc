//! Integration tests: `class` declarations, constructors, and `new` via the CLI.

mod common;
use common::jsc;

/// Class basics run inline with `-p` through the built binary.
///
/// A single class (no inheritance) with a constructor: `new A(args)` allocates a
/// fresh instance, binds `this` to it, runs the constructor for its side effects,
/// and evaluates to the instance. A class with no constructor gets a default
/// no-op one, and every instance is a plain object (`typeof` "object").
#[test]
fn class_basics_via_eval() {
    // Constructor stores a parameter onto `this`; the field reads back.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{constructor(x){this.v=x}} let a=new A(42); a.v"
        ]),
        "42"
    );
    // Two fields set from constant initializers, summed after construction.
    assert_eq!(
        jsc(&[
            "-p",
            "class P{constructor(){this.x=40; this.y=2}} let p=new P(); p.x+p.y"
        ]),
        "42"
    );
    // A fresh instance is an object.
    assert_eq!(
        jsc(&["-p", "class A{constructor(x){this.v=x}} typeof (new A(1))"]),
        "object"
    );
    // An empty class (no constructor => default no-op ctor) still yields an object.
    assert_eq!(jsc(&["-p", "class E{} let e=new E(); typeof e"]), "object");
}

/// A multi-parameter constructor stores each argument onto its own field; the
/// fields read back independently after construction.
#[test]
fn class_multi_field_constructor_via_eval() {
    assert_eq!(
        jsc(&[
            "-p",
            "class Point{constructor(a,b){this.x=a; this.y=b}} let p=new Point(3,4); p.x+p.y"
        ]),
        "7"
    );
    // A field left unset by the constructor reads back `undefined`.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{constructor(){this.a=1}} let o=new A(); typeof o.b"
        ]),
        "undefined"
    );
}

/// The instance is an ordinary heap object after construction: its fields are
/// mutable, and it composes as a value inside other containers / as an argument
/// to another `new`.
#[test]
fn class_instance_is_a_plain_object_via_eval() {
    // The instance's fields are writable after construction (`a.v = 99`).
    assert_eq!(
        jsc(&[
            "-p",
            "class A{constructor(){this.v=1}} let a=new A(); a.v=99; a.v"
        ]),
        "99"
    );
    // An instance held by an object, reached through a property hop.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{constructor(x){this.v=x}} let o={a:new A(7)}; o.a.v"
        ]),
        "7"
    );
    // A conditional field assignment in the constructor still slots the field.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{constructor(c){if(c){this.v=5}else{this.v=0}}} let a=new A(1); a.v"
        ]),
        "5"
    );
    // One instance passed as another constructor's argument, read through two hops.
    assert_eq!(
        jsc(&[
            "-p",
            "class B{constructor(n){this.n=n}} class A{constructor(b){this.b=b}} \
             let a=new A(new B(3)); a.b.n"
        ]),
        "3"
    );
}

/// A class declared inside a function is bound in that function's scope; `new`
/// works from there and the instance escapes as the function's return value. A
/// constructor that reads (but does not arithmetically combine) a captured outer
/// binding sees it through the closure environment.
#[test]
fn class_in_function_scope_via_eval() {
    assert_eq!(
        jsc(&[
            "-p",
            "function mk(x){ class A{constructor(v){this.v=v}} return new A(x); } mk(9).v"
        ]),
        "9"
    );
    // The constructor captures an outer binding and stores it onto `this`.
    assert_eq!(
        jsc(&[
            "-p",
            "let base=40; class A{constructor(){this.v=base}} let a=new A(); a.v"
        ]),
        "40"
    );
    // `new` reached through an arrow that captures the class binding.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{constructor(x){this.v=x}} let f=()=>new A(8); f().v"
        ]),
        "8"
    );
}

/// The class NAME is resolvable inside its own constructor and method
/// bodies, so `new C()` works everywhere — not only at the class's declaration
/// scope. The name is captured through the same boxed-cell path an arrow uses,
/// and reads back the fully-built class value when the method later runs.
#[test]
fn class_name_resolvable_in_method_bodies() {
    // A method constructs a fresh instance of its own class and returns it.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{constructor(v){this.v=v} mk(){return new C(this.v+1)}} new C(41).mk().v"
        ]),
        "42"
    );
    // A method that recurses through `new` of its own class (5! = 120).
    assert_eq!(
        jsc(&[
            "-p",
            "class F{constructor(n){this.n=n} fact(){ if(this.n<=1){return 1} \
             return this.n * new F(this.n-1).fact() }} new F(5).fact()"
        ]),
        "120"
    );
    // A factory method builds a linked node chain (each `cons` `new`s its class),
    // walked back by field hops: 3 + 2 + 1 == 6.
    assert_eq!(
        jsc(&[
            "-p",
            "class N{constructor(v){this.v=v; this.next=undefined} \
             cons(v){let n=new N(v); n.next=this; return n}} \
             let a=new N(1); let b=a.cons(2); let c=b.cons(3); c.v+c.next.v+c.next.next.v"
        ]),
        "6"
    );
    // The builtin `new TypeError(..)` path is unchanged inside a method body.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{constructor(){} boom(){ return (new TypeError('x')).message }} new C().boom()"
        ]),
        "x"
    );
}

/// Instance methods live on the class's ONE shared prototype
/// object, resolved by the own→prototype fallback when the method name is absent
/// as an own property of the instance. Own fields still take precedence.
#[test]
fn class_methods_via_eval() {
    // A method reads an own field set by the constructor.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{constructor(){this.n=41} inc(){return this.n+1}} new C().inc()"
        ]),
        "42"
    );
    // Method-calls-method: `this.two()` resolves `two` on the prototype too, with
    // `this` still bound to the same instance.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{constructor(){this.a=6} two(){return this.a} big(){return this.two()*7}} \
             new C().big()"
        ]),
        "42"
    );
    // Multiple instances share the one method but observe their own state.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{constructor(x){this.n=x} get2(){return this.n}} \
             let a=new C(5); let b=new C(9); a.get2()+b.get2()"
        ]),
        "14"
    );
    // A method value read off an instance is a function.
    assert_eq!(
        jsc(&["-p", "class C{m(){return 1}} typeof (new C().m)"]),
        "function"
    );
}

/// A method taking arguments, and a method whose name coexists with a same-named
/// own field lookup elsewhere — the own field wins on a field read while the
/// method still resolves through the prototype.
#[test]
fn class_method_args_and_precedence_via_eval() {
    // Method arguments flow through the uniform ABI like any call.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{constructor(){this.base=10} add(x){return this.base+x}} new C().add(5)"
        ]),
        "15"
    );
    // An own field shadows nothing here, but reading an absent own name that is a
    // method returns the function; reading an own field returns the field value.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{constructor(){this.v=3} m(){return this.v*2}} \
             let c=new C(); c.v + c.m()"
        ]),
        "9"
    );
}

/// Regression: a constructor field init `this.x = a+b` combines its parameters
/// through the child scratch base; the dedicated high slots keep the stashed
/// `this` receiver/key intact across the arithmetic RHS.
#[test]
fn ctor_field_computed_init_via_eval() {
    assert_eq!(
        jsc(&[
            "-p",
            "class P{constructor(a,b){this.x=a+b}} let p=new P(3,4); p.x"
        ]),
        "7"
    );
}

/// Instance field declarations (`field = init`). A field is an
/// instance-level own property initialized on construction, BEFORE the
/// constructor body runs. The instance shape is the union of the field names and
/// the constructor's `this.<f> = ..` assignments.
#[test]
fn class_fields_via_eval() {
    // A single field with a constant initializer, no constructor: a default
    // constructor is synthesized from the field initializers.
    assert_eq!(jsc(&["-p", "class A{ v = 42 } new A().v"]), "42");
    // The field initializer runs BEFORE the constructor body: the constructor
    // observes the already-initialized `base` and bumps it.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{ base = 40; constructor(){ this.base = this.base + 2 } } new A().base"
        ]),
        "42"
    );
    // A field initializer plus a method reading `this.<field>` through the
    // prototype.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{ n = 41; inc(){ return this.n + 1 } } new A().inc()"
        ]),
        "42"
    );
    // Multiple fields, a constructor argument mutating one of them, and two
    // independent instances.
    assert_eq!(
        jsc(&[
            "-p",
            "class P{ x = 40; y = 2; constructor(k){ this.x = this.x + k } } \
             new P(0).x + new P(0).y"
        ]),
        "42"
    );
}

/// A later field initializer may read an earlier field through `this`
/// (field inits are evaluated in declaration order), an uninitialized field (`f;`)
/// reads back `undefined`, and a field-only class (no constructor) still yields a
/// plain object.
#[test]
fn class_field_ordering_and_undefined_via_eval() {
    // A later field reads an earlier one through `this` (declaration order).
    assert_eq!(
        jsc(&["-p", "class A{ a = 40; b = this.a + 2 } new A().b"]),
        "42"
    );
    // An uninitialized field initializes to `undefined`.
    assert_eq!(
        jsc(&["-p", "class A{ x; } typeof (new A().x)"]),
        "undefined"
    );
    // A field-only class is still a plain object.
    assert_eq!(jsc(&["-p", "class A{ v = 1 } typeof (new A())"]), "object");
    // The union shape: a field the constructor never assigns coexists with a field
    // only the constructor assigns.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ a = 5; constructor(){ this.b = 7 } } let c=new C(); c.a + c.b"
        ]),
        "12"
    );
}

/// Combined acceptance list run end-to-end with no output drift: object-literal
/// methods, `class`/`constructor`/`new`, prototype-shared methods, and class
/// fields together.
#[test]
fn objects_and_classes_acceptance_via_eval() {
    // Object-literal methods bind `this`.
    assert_eq!(
        jsc(&["-p", "let o={n:41, inc(){return this.n+1}}; o.inc()"]),
        "42"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let o={a:2,b:3, sum(){return this.a+this.b}, scaled(){return this.sum()*8}}; o.scaled()"
        ]),
        "40"
    );
    assert_eq!(jsc(&["-p", "let n=7; let o={n}; o.n"]), "7");
    // Class, constructor, new.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{constructor(x){this.v=x}} let a=new A(42); a.v"
        ]),
        "42"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "class P{constructor(){this.x=40; this.y=2}} let p=new P(); p.x+p.y"
        ]),
        "42"
    );
    assert_eq!(
        jsc(&["-p", "class A{constructor(x){this.v=x}} typeof (new A(1))"]),
        "object"
    );
    // Prototype-shared methods, including a method calling another method.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{constructor(){this.n=41} inc(){return this.n+1}} new C().inc()"
        ]),
        "42"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "class C{constructor(){this.a=6} two(){return this.a} big(){return this.two()*7}} new C().big()"
        ]),
        "42"
    );
    // Class fields, including a field init reading an earlier field.
    assert_eq!(jsc(&["-p", "class A{ v = 42 } new A().v"]), "42");
    assert_eq!(
        jsc(&[
            "-p",
            "class A{ base = 40; constructor(){ this.base = this.base + 2 } } new A().base"
        ]),
        "42"
    );
}

/// L3 integration (Gap 1 + Gap 2 together): a single class method that BOTH
/// `new C()`s its own class (the class name resolves inside the method body —
/// Gap 1) AND closes over an enclosing top-level `let`/`const` (Gap 2), running
/// end to end through the built binary. Exercises the combined path in one
/// method so a regression in either capture route is caught here.
#[test]
fn class_method_news_own_class_and_captures_enclosing_binding_via_eval() {
    // The method reads an enclosing `let` and `new`s its own class with it: the
    // class name and the outer binding are both captured through the boxed-cell
    // path, and 40 + 2 == 42.
    assert_eq!(
        jsc(&[
            "-p",
            "let base=40; class C{ constructor(v){this.v=v} mk(){ return new C(base + this.v) } } \
             new C(2).mk().v"
        ]),
        "42"
    );
    // The method WRITES back to the enclosing `let` and `new`s its own class:
    // two calls bump the shared cell to 2, observed after the calls return.
    assert_eq!(
        jsc(&[
            "-p",
            "let count=0; class C{ constructor(){} bump(){ count = count + 1; return new C() } } \
             let c=new C(); c.bump(); c.bump(); count"
        ]),
        "2"
    );
    // Enclosing `const` read + `new` of the own class, method-calls-method: `mk`
    // resolves the class name, `total` reads the captured const (10 + 32 == 42).
    assert_eq!(
        jsc(&[
            "-p",
            "const bonus=10; class C{ constructor(n){this.n=n} mk(){ return new C(this.n) } \
             total(){ return bonus + this.mk().n } } new C(32).total()"
        ]),
        "42"
    );
}

/// `extends`: a derived class inherits its parent's
/// methods through the shared-prototype chain, `super(..)` runs the parent
/// constructor on the same instance, `super.m(..)` calls the parent's method, and
/// `instanceof` walks the whole prototype chain.
#[test]
fn class_extends_via_eval() {
    // super(n) forwards to the parent constructor, which sets the inherited field.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{constructor(n){this.n=n}} class B extends A{constructor(n){super(n)}} \
             new B(\"x\").n",
        ]),
        "x",
    );
    // A subclass with no constructor reuses the parent's (an implicit forwarding
    // ctor): the argument reaches the parent field and inherited methods resolve.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{constructor(n){this.n=n} dbl(){return this.n*2}} class B extends A{} \
             new B(5).dbl()",
        ]),
        "10",
    );
    // A method override shadows the inherited one; `super.m()` still reaches it.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{greet(){return \"A\"}} class B extends A{greet(){return super.greet()+\"B\"}} \
             new B().greet()",
        ]),
        "AB",
    );
    // A three-level `super` chain accumulates through each ancestor.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{m(){return 1}} class B extends A{m(){return super.m()+10}} \
             class C extends B{m(){return super.m()+100}} new C().m()",
        ]),
        "111",
    );
    // Own and inherited fields coexist on the instance shape.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{constructor(a){this.a=a}} \
             class B extends A{constructor(a,b){super(a); this.b=b}} \
             let o=new B(1,2); o.a+o.b",
        ]),
        "3",
    );
    // An inherited method reads a field set by the parent constructor.
    assert_eq!(
        jsc(&[
            "-p",
            "class Animal{constructor(nm){this.name=nm} speak(){return this.name+\" noise\"}} \
             class Dog extends Animal{speak(){return this.name+\" barks\"}} \
             new Dog(\"Rex\").speak()",
        ]),
        "Rex barks",
    );
}

/// `instanceof` across an `extends` chain matches every ancestor and nothing off
/// the chain, and a derived instance is still `typeof` "object".
#[test]
fn class_extends_instanceof_via_eval() {
    let prelude = "class A{} class B extends A{} class C extends B{} class Z{} ";
    assert_eq!(
        jsc(&["-p", &format!("{prelude} new C() instanceof A")]),
        "true"
    );
    assert_eq!(
        jsc(&["-p", &format!("{prelude} new C() instanceof B")]),
        "true"
    );
    assert_eq!(
        jsc(&["-p", &format!("{prelude} new C() instanceof C")]),
        "true"
    );
    assert_eq!(
        jsc(&["-p", &format!("{prelude} new A() instanceof C")]),
        "false"
    );
    assert_eq!(
        jsc(&["-p", &format!("{prelude} new C() instanceof Z")]),
        "false"
    );
    // A non-instance left operand never matches (no trap).
    assert_eq!(jsc(&["-p", &format!("{prelude} 5 instanceof A")]), "false");
    assert_eq!(
        jsc(&["-p", &format!("{prelude} typeof new C() === \"object\"")]),
        "true"
    );
}

/// Static members live on the class VALUE: `static m(){}` is read as `C.m()` and
/// `static f = init` as `C.f`. A static method binds `this` to the class, so it
/// can read another static member off `this`; an uninitialized `static f;` reads
/// back `undefined`. Static and instance members coexist on the same class.
#[test]
fn class_static_members_via_eval() {
    // A static method called off the class name.
    assert_eq!(
        jsc(&["-p", "class C{ static make(){ return 42 } } C.make()"]),
        "42"
    );
    // A static method with parameters.
    assert_eq!(
        jsc(&["-p", "class M{ static add(a,b){ return a+b } } M.add(3,4)"]),
        "7"
    );
    // A static field read off the class name.
    assert_eq!(jsc(&["-p", "class C{ static count = 7 } C.count"]), "7");
    // A static field with an expression initializer, evaluated once at definition.
    assert_eq!(jsc(&["-p", "class C{ static v = 3*10 + 2 } C.v"]), "32");
    // An uninitialized static field reads back `undefined`.
    assert_eq!(jsc(&["-p", "class C{ static f; } typeof C.f"]), "undefined");
    // A static method reads a static field through `this` (bound to the class).
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ static n = 5; static getN(){ return this.n } } C.getN()"
        ]),
        "5"
    );
    // A static method calls a sibling static through the class name.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ static a(){ return 10 } static b(){ return C.a()+5 } } C.b()"
        ]),
        "15"
    );
    // Static and instance members coexist: an instance method and a static method
    // on the same class, read from their respective receivers.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ constructor(){this.x=1} static y=2; inst(){return this.x} static st(){return this.y} } \
             let c=new C(); c.inst()+C.st()"
        ]),
        "3"
    );
}

/// A static field initializer that is NOT a bare literal — it references the class,
/// an earlier static, or `this` — is assigned AFTER the class binding exists, in
/// source declaration order, so a later static observes the earlier ones. `this`
/// inside such an initializer is the class itself. Literal initializers stay inline.
#[test]
fn class_static_field_computed_init_via_eval() {
    // References an earlier static through the class name.
    assert_eq!(
        jsc(&["-p", "class C{ static a = 1; static b = C.a + 1 } C.b"]),
        "2"
    );
    assert_eq!(
        jsc(&["-p", "class C{ static a = 2; static b = C.a * 3 } C.b"]),
        "6"
    );
    // Chained: each line sees the previous one already assigned.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ static a=1; static b=C.a+1; static c=C.b+1 } C.c"
        ]),
        "3"
    );
    // `this` inside a static initializer is the class.
    assert_eq!(
        jsc(&["-p", "class C{ static a = 5; static b = this.a + 10 } C.b"]),
        "15"
    );
    // A literal initializer alongside a computed one: both resolve, in order.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ static a=1; static b=\"x\"; static c=C.a+2 } C.a + C.c"
        ]),
        "4"
    );
    // A computed static is still NOT visible on an instance.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ static a=1; static b=C.a+1 } typeof (new C()).b"
        ]),
        "undefined"
    );
}

/// Instance accessors. A `get x(){}` / `set x(v){}` lives on the shared
/// prototype as a `TYPE_ACCESSOR` box: reading `obj.x` invokes the getter with
/// `this` = the instance; writing `obj.x = v` invokes the setter. A half-declared
/// accessor degrades cleanly — reading a set-only property is `undefined`, and
/// assigning a get-only property throws a `TypeError` (strict `OrdinarySet`).
#[test]
fn class_instance_accessor_via_eval() {
    // A getter computes from fields.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ constructor(w,h){this.w=w; this.h=h} get area(){return this.w*this.h} } \
             new C(3,4).area"
        ]),
        "12"
    );
    // A getter+setter pair: the setter transforms and stores; the getter reads back.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ constructor(){this._v=0} get v(){return this._v} set v(x){this._v=x*2} } \
             let c=new C(); c.v=5; c.v"
        ]),
        "10"
    );
    // Assigning a get-only accessor throws and leaves it unchanged.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ get x(){return 7} } let c=new C(); let r; try { c.x=99 } catch(e) { r=e.name } r+c.x"
        ]),
        "TypeError7"
    );
    // Reading a set-only accessor yields `undefined`.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ constructor(){this.k=1} set y(v){this.k=v} } typeof (new C()).y"
        ]),
        "undefined"
    );
    // A setter's side effect is observable through a plain field.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ constructor(){this.k=0} set y(v){this.k=v+1} } let c=new C(); c.y=41; c.k"
        ]),
        "42"
    );
    // A computed member read `c[key]` also fires the getter.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ get z(){return 42} } let c=new C(); let k=\"z\"; c[k]"
        ]),
        "42"
    );
    // Two accessors with distinct names coexist on one prototype.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ constructor(){this._a=1;this._b=2} get a(){return this._a} get b(){return this._b} } \
             let c=new C(); c.a+\",\"+c.b"
        ]),
        "1,2"
    );
    // Compound assignment through an accessor reads via the getter and writes via
    // the setter.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ constructor(){this._n=10} get n(){return this._n} set n(v){this._n=v} } \
             let c=new C(); c.n += 5; c.n"
        ]),
        "15"
    );
    // A getter body may call an instance method.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ constructor(){this.r=2} area(){return this.r*this.r} get a2(){return this.area()+1} } \
             new C().a2"
        ]),
        "5"
    );
    // A getter returning a function is itself callable: `c.f()` reads (invokes) the
    // getter, then calls its result.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ get f(){ return function(){ return 99 } } } new C().f()"
        ]),
        "99"
    );
}

/// An accessor declared on a base class is inherited through the prototype chain,
/// so a derived instance reads/writes it just like an own accessor.
#[test]
fn class_inherited_accessor_via_eval() {
    // A subclass instance reads a getter defined on the parent's prototype.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{ get name(){return \"A\"+this.n} } class B extends A{ constructor(){super(); this.n=5} } \
             new B().name"
        ]),
        "A5"
    );
    // A getter+setter pair inherited from the base drives a subclass field.
    assert_eq!(
        jsc(&[
            "-p",
            "class A{ get v(){return this._v} set v(x){this._v=x+1} } \
             class B extends A{ constructor(){super(); this._v=0} } \
             let b=new B(); b.v=10; b.v"
        ]),
        "11"
    );
}

/// An accessor that throws propagates like any other call: a `try`/`catch` around
/// the member read/write observes the thrown value at the correct handler depth.
#[test]
fn class_throwing_accessor_via_eval() {
    // A throwing getter is caught by an enclosing `try` (the caught message is
    // routed into a binding read as the trailing expression).
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ get bad(){ throw new Error(\"boom\") } } let c=new C(); \
             let m=\"none\"; try { c.bad } catch(e){ m=e.message } m"
        ]),
        "boom"
    );
    // A throwing setter is caught by an enclosing `try`.
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ set bad(v){ throw new Error(\"set-boom\") } } let c=new C(); \
             let m=\"none\"; try { c.bad=1 } catch(e){ m=e.message } m"
        ]),
        "set-boom"
    );
}

/// Class constructors and methods tolerate missing and extra arguments: a
/// missing one is `undefined`, a surplus one is ignored.
#[test]
fn class_calls_tolerate_missing_and_extra_arguments() {
    assert_eq!(
        jsc(&[
            "-p",
            "class P { constructor(x, y){ this.y = y } } typeof new P(1).y"
        ]),
        "undefined"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "class P { constructor(x){ this.x = x } } new P(1, 2, 3).x"
        ]),
        "1"
    );
    assert_eq!(
        jsc(&["-p", "class C { m(a, b){ return b } } new C().m(1)"]),
        "undefined"
    );
    assert_eq!(
        jsc(&["-p", "class C { m(a){ return a } } new C().m(4, 5)"]),
        "4"
    );
}

/// Instances are dynamic: writing a field the constructor never set adds an
/// own property, and assigning a prototype method's name shadows it on the
/// instance. Accessor setters run; a get-only accessor rejects the write with
/// a `TypeError` (strict `OrdinarySet`).
#[test]
fn class_instance_property_writes() {
    let wrap = |body: &str| {
        format!(
            "let r=\"none\"; try {{ {body} }} catch(e) {{ r=(e instanceof TypeError)+\"|\"+e.message }} r"
        )
    };
    assert_eq!(
        jsc(&[
            "-p",
            "class C { constructor(){ this.a = 1 } } let c = new C(); c.z = 3; c.a + c.z"
        ]),
        "4"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "class C { m(){ return 1 } } let c = new C(); let d = new C(); c.m = 5; c.m + d.m()"
        ]),
        "6"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ constructor(){this.k=0} set y(v){this.k=v+1} } let c=new C(); c.y=41; c.k"
        ]),
        "42"
    );
    assert_eq!(
        jsc(&[
            "-p",
            &wrap("class C{ get x(){return 7} } let c=new C(); c.x=99; r=c.x")
        ]),
        "true|Cannot set property x of object which has only a getter"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "class C{ constructor(){this.k=0} set y(v){this.k=v} } let c=new C(); c.w=1; c.w"
        ]),
        "1"
    );
}
