//! Layout pins: the heap-block, property-table and symbol layouts in
//! [`super`] must match `DESIGN.md` §5 and runtime-memory.md §3.4.

use super::{
    ARRAY_ELEMS_OFFSET, ARRAY_FLAG_SPARSE, ARRAY_LEN_OFFSET, ATTR_ACCESSOR, ATTR_ENUMERABLE,
    CLASS_ITERATOR, CLASS_SYMBOL, DATE_MS_OFFSET, ELEMS_DATA_OFFSET, FUNC_INDEX_OFFSET,
    GEN_DONE_OFFSET, HEADER_FLAGS_OFFSET, MAP_SIZE_OFFSET, OBJ_PREFIX_SIZE, OBJ_PROTO_OFFSET,
    PROMISE_BLOCK_SIZE, PROMISE_STATE_OFFSET, PROMISE_VALUE_OFFSET, PROP_ATTRS_OFFSET,
    PROP_ENTRY_SIZE, PROP_KEY_OFFSET, PROP_VALUE_OFFSET, PROPS_COUNT_OFFSET, PROPS_HAS_INDEX_KEYS,
    PROPS_HAS_SYMBOL_KEYS, PROPS_HEADER_SIZE, PROPS_TFLAGS_OFFSET, STR_FLAG_ARRAY_INDEX,
    STR_FLAG_TWO_BYTE, SYMBOL_BLOCK_SIZE, SYMBOL_DESCRIPTION_OFFSET, SYMBOL_FLAG_REGISTERED,
    TYPE_STRING, TYPE_SYMBOL,
};

/// Pins the object-family prefix and per-kind payload layouts to
/// `DESIGN.md` sections 5.1-5.2.
#[test]
fn object_family_layout_matches_design() {
    use super::{
        ARRAY_BLOCK_SIZE, DATE_BLOCK_SIZE, ELEMS_CAP_OFFSET, FUNC_BLOCK_SIZE, FUNC_ENV_OFFSET,
        GEN_BLOCK_SIZE, GEN_ENV_OFFSET, GEN_SM_IDX_OFFSET, GEN_STATE_OFFSET, MAP_BLOCK_SIZE,
        MAP_ENTRIES_OFFSET, OBJ_PROPS_OFFSET, PROMISE_FLAG_HANDLED, PROMISE_FLAG_IN_REJECTED_LIST,
        PROMISE_REACTIONS_OFFSET, PROMISE_REJECTED_NEXT_OFFSET, PROMISE_SEQ_OFFSET,
        REACTION_BLOCK_SIZE, REACTION_KIND_OFFSET, REACTION_NEXT_OFFSET, REACTION_SLOTS,
        TYPE_ELEMS,
    };
    // TYPE_PROMISE (runtime-memory.md §3.4): reactions @20, seq @32,
    // rejected_next @36; flags HANDLED = 2, IN_REJECTED_LIST = 4; the reaction
    // record is a TYPE_ENV of 6 slots, kind in slot 3, next in slot 5.
    assert_eq!(PROMISE_REACTIONS_OFFSET, 20);
    assert_eq!(PROMISE_SEQ_OFFSET, 32);
    assert_eq!(PROMISE_REJECTED_NEXT_OFFSET, 36);
    assert_eq!(
        (PROMISE_FLAG_HANDLED, PROMISE_FLAG_IN_REJECTED_LIST),
        (2, 4)
    );
    assert_eq!(REACTION_SLOTS, 6);
    assert_eq!(REACTION_BLOCK_SIZE, 56);
    assert_eq!(REACTION_KIND_OFFSET, 8 + 8 * 3);
    assert_eq!(REACTION_NEXT_OFFSET, 8 + 8 * 5);
    let actual = [
        ("OBJ_PROPS_OFFSET", OBJ_PROPS_OFFSET),
        ("OBJ_PROTO_OFFSET", OBJ_PROTO_OFFSET),
        ("OBJ_PREFIX_SIZE", OBJ_PREFIX_SIZE),
        ("FUNC_INDEX_OFFSET", FUNC_INDEX_OFFSET),
        ("FUNC_ENV_OFFSET", FUNC_ENV_OFFSET),
        ("FUNC_BLOCK_SIZE", FUNC_BLOCK_SIZE),
        ("MAP_SIZE_OFFSET", MAP_SIZE_OFFSET),
        ("MAP_ENTRIES_OFFSET", MAP_ENTRIES_OFFSET),
        ("MAP_BLOCK_SIZE", MAP_BLOCK_SIZE),
        ("PROMISE_STATE_OFFSET", PROMISE_STATE_OFFSET),
        ("PROMISE_VALUE_OFFSET", PROMISE_VALUE_OFFSET),
        ("PROMISE_BLOCK_SIZE", PROMISE_BLOCK_SIZE),
        ("GEN_SM_IDX_OFFSET", GEN_SM_IDX_OFFSET),
        ("GEN_STATE_OFFSET", GEN_STATE_OFFSET),
        ("GEN_DONE_OFFSET", GEN_DONE_OFFSET),
        ("GEN_ENV_OFFSET", GEN_ENV_OFFSET),
        ("GEN_BLOCK_SIZE", GEN_BLOCK_SIZE),
        ("DATE_MS_OFFSET", DATE_MS_OFFSET),
        ("DATE_BLOCK_SIZE", DATE_BLOCK_SIZE),
        ("ARRAY_LEN_OFFSET", ARRAY_LEN_OFFSET),
        ("ARRAY_ELEMS_OFFSET", ARRAY_ELEMS_OFFSET),
        ("ARRAY_BLOCK_SIZE", ARRAY_BLOCK_SIZE),
        ("ELEMS_CAP_OFFSET", ELEMS_CAP_OFFSET),
        ("ELEMS_DATA_OFFSET", ELEMS_DATA_OFFSET),
    ];
    let design = [
        4, 8, 16, // prefix
        16, 24, 32, // function
        16, 24, 32, // map / set
        16, 24, 40, // promise
        16, 20, 24, 32, 48, // generator
        16, 24, // date
        16, 20, 24, // array handle
        4, 8, // element store
    ];
    for ((name, got), want) in actual.iter().zip(design) {
        assert_eq!(*got, want, "{name}");
    }
    // Every i64/f64 slot is 8-byte aligned; every block size is a BLOCK_ALIGN multiple.
    assert_eq!(TYPE_ELEMS, 13);
    for slot in [
        ELEMS_DATA_OFFSET,
        OBJ_PROTO_OFFSET,
        FUNC_ENV_OFFSET,
        MAP_ENTRIES_OFFSET,
        PROMISE_VALUE_OFFSET,
        GEN_ENV_OFFSET,
        DATE_MS_OFFSET,
    ] {
        assert_eq!(slot % 8, 0);
    }
    for size in [
        ARRAY_BLOCK_SIZE,
        FUNC_BLOCK_SIZE,
        MAP_BLOCK_SIZE,
        PROMISE_BLOCK_SIZE,
        GEN_BLOCK_SIZE,
        DATE_BLOCK_SIZE,
    ] {
        assert_eq!(size % super::BLOCK_ALIGN, 0);
    }
}

/// Pins the `TYPE_PROPS` table layout, attribute bits, and flag bits to
/// `DESIGN.md` sections 5.1-5.3.
#[test]
fn property_table_layout_matches_design() {
    use super::{
        ARRAY_FLAG_DICTIONARY, ARRAY_FLAG_JOINING, ARRAY_FLAG_LENGTH_READONLY, ATTR_CONFIGURABLE,
        ATTR_WRITABLE, ATTRS_DEFAULT, OBJ_FLAG_NOT_EXTENSIBLE, PROPS_CAP_OFFSET,
        PROPS_HAS_NON_ENUM, PROPS_LIVE_OFFSET, STR_FLAG_INTERNED, TYPE_PROPS,
    };
    assert_eq!(TYPE_PROPS, 12);
    assert_eq!(
        [
            PROPS_TFLAGS_OFFSET,
            PROPS_COUNT_OFFSET,
            PROPS_CAP_OFFSET,
            PROPS_LIVE_OFFSET,
            PROPS_HEADER_SIZE
        ],
        [2, 4, 8, 12, 16]
    );
    assert_eq!(
        [
            PROP_ENTRY_SIZE,
            PROP_KEY_OFFSET,
            PROP_ATTRS_OFFSET,
            PROP_VALUE_OFFSET
        ],
        [16, 0, 4, 8]
    );
    assert_eq!(
        [
            ATTR_WRITABLE,
            ATTR_ENUMERABLE,
            ATTR_CONFIGURABLE,
            ATTR_ACCESSOR
        ],
        [1, 2, 4, 8]
    );
    assert_eq!(ATTRS_DEFAULT, 0b0111);
    assert_eq!(
        [
            PROPS_HAS_INDEX_KEYS,
            PROPS_HAS_NON_ENUM,
            super::PROPS_WATCHED,
            PROPS_HAS_SYMBOL_KEYS
        ],
        [1, 2, 4, 8]
    );
    assert_eq!(
        [STR_FLAG_INTERNED, STR_FLAG_ARRAY_INDEX, STR_FLAG_TWO_BYTE],
        [1, 2, 4]
    );
    assert_eq!(OBJ_FLAG_NOT_EXTENSIBLE, 1);
    assert_eq!(
        [
            ARRAY_FLAG_SPARSE,
            ARRAY_FLAG_JOINING,
            ARRAY_FLAG_LENGTH_READONLY,
            ARRAY_FLAG_DICTIONARY
        ],
        [4, 8, 16, 32]
    );
    assert_eq!(HEADER_FLAGS_OFFSET, 2);
}

/// Pins the `TYPE_SYMBOL` block layout and the `typeof` word table to
/// DESIGN.md §5.2 and §6.5.
#[test]
fn symbol_layout_matches_design() {
    use super::{BLOCK_ALIGN, CLASS_GLOBAL, STR_FLAG_INTERNED, TYPE_ELEMS, TYPEOF_STRINGS};
    assert_eq!(TYPE_SYMBOL, TYPE_ELEMS + 1);
    assert_eq!([SYMBOL_DESCRIPTION_OFFSET, SYMBOL_BLOCK_SIZE], [8, 16]);
    assert_eq!(SYMBOL_BLOCK_SIZE % BLOCK_ALIGN, 0);
    // Bits 1 and 2 (the string INTERNED / ARRAY_INDEX bits, tested on any key
    // block) stay clear. Bit 4 is shared with STR_FLAG_TWO_BYTE, which only
    // string paths read, after the TYPE byte says TYPE_STRING.
    assert_eq!(SYMBOL_FLAG_REGISTERED, 4);
    assert_eq!(
        SYMBOL_FLAG_REGISTERED & (STR_FLAG_ARRAY_INDEX | STR_FLAG_INTERNED),
        0
    );
    assert_eq!(SYMBOL_FLAG_REGISTERED, STR_FLAG_TWO_BYTE);
    // Section 7.6: the private-name flag, clear of the string key bits.
    assert_eq!(super::SYMBOL_FLAG_PRIVATE, 8);
    assert_eq!(
        super::SYMBOL_FLAG_PRIVATE
            & (STR_FLAG_ARRAY_INDEX | STR_FLAG_INTERNED | SYMBOL_FLAG_REGISTERED),
        0
    );
    assert_ne!(TYPE_SYMBOL, TYPE_STRING);
    assert_eq!(CLASS_SYMBOL, CLASS_GLOBAL + 1);
    // Section 5.4: the iterator objects' class byte.
    assert_eq!(CLASS_ITERATOR, CLASS_SYMBOL + 1);
    assert_eq!(TYPEOF_STRINGS[6], "symbol");
}
