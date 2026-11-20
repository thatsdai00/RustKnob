# Device protocols

RustKnob reads the battery of wireless devices through these files. The protocols are not built into the exe.
When a device is plugged in, the app looks its `VID:PID` up in `index.json`. It then fetches that device's
family file, once, and keeps it in `%LOCALAPPDATA%\RustKnob\devices`. The program in the file is run by
`src/battery/script.rs`.

To add a device, put its product in a family file and in `index.json`, and raise the family's version there
(installed copies fetch it again). Then run `cargo test --release devices`.

## index.json

```json
{
  "families": { "rongyuan": 1 },
  "order": ["razer", "rongyuan"],
  "ids": { "3151:5038": "rongyuan", "1532:*": "razer" }
}
```

- `families`: the version of each file. Raise it when the file changes.
- `order`: the order in which families claim products. When two families share a vendor, the more specific one
  goes first.
- `ids`: an exact `VID:PID`, or `VID:*` for every product of a vendor that no earlier family claimed. Hex digits
  are upper case.

## A family file

```json
{
  "v": 1,
  "devices": [["3151:5038", "f7", {"kb": "MonsGeek keyboard"}, "keyboard", {"var": 1}]],
  "except": ["373B:11CA"],
  "curves": { "liion": [[4186, 100], [3500, 0]] },
  "subs": { "name": [ /* steps */ ] },
  "protocols": { "f7": { /* below */ } },
  "tests": [ /* below */ ]
}
```

- `v`: the file's version. The index holds the same number.
- `devices`: one entry per product, `[id, protocol, name, kind, vars]`.
  - `id`: `"VID:PID"`, or `"VID:*"` for every product of that vendor.
  - `name`: a string, or an object keyed by slot (`{"kb": "...", "ms": "..."}`). Use `""` for the product string.
  - `kind`: `mouse`, `keyboard`, `headset`, `gamepad`, `phone` or `other`.
  - `vars`: numbers the program can read as `$name`. `$vid` and `$pid` are always set.
- `except`: products never to be sent anything.
- `curves`: falling `[millivolts, percent]` points, used by `{"curve": [name, mV]}`.
- `subs`: named blocks of steps, run with `{"call": name}`.

### A protocol

```json
{
  "pick": [{"mi": 2, "page": "FFFF", "usage": "0002", "feat": [7]}],
  "open": "feature",
  "every": 10, "asleep": 30, "grace": 1800,
  "kind": "mouse",
  "strip": [" Receiver", " Dongle"],
  "run": [ /* steps */ ]
}
```

- `pick`: which HID collections to use, best first.
  - Each selector can set `mi` (the interface number), `page` and `usage` (hex strings), and `out`, `in` and `feat`.
  - `out`, `in` and `feat` are report lengths with the report ID included. Give an exact number or `[min, max]`;
    either bound of the range can be left out.
  - Collections are tried in order, and the first one whose run emits something is kept. If none emits anything,
    the first one that opens is kept, and its device is treated as asleep.
- `open`: `rw` (the default; input and output reports), `read`, or `feature` (no rights; feature reports only,
  which is what a mouse or keyboard collection allows).
- `every`: seconds between two reads while the device is awake. The default is 10.
- `asleep`: seconds between two reads while it is asleep. The default is 30.
- `grace`: seconds a device that stopped answering keeps its last level, dimmed. The default is 600.
- `strip`: endings removed from the product string before it is used as the name.

### Steps

A step is an object whose first key is the operation. When a step fails, the block it is in stops there.

| step | what it does |
|---|---|
| `{"out": "08 03 $x", "ck": CK}` | Writes an output report (interrupt OUT). |
| `{"oset": "...", "ck": CK}` | Sends an output report over the control pipe (`HidD_SetOutputReport`). |
| `{"fset": "...", "ck": CK}` | Sends a feature report. |
| `{"fget": id}` | Reads feature report `id` into the reply. Byte 0 is the report ID. |
| `{"iget": id}` | Reads input report `id` over the control pipe into the reply. Byte 0 is the report ID. |
| `{"read": ms, "if": COND}` | Reads input reports until one meets `COND`, or fails after `ms`. A leading `00` report ID is dropped, so a numbered report keeps its ID at byte 0. |
| `{"flush": 1}` | Discards the input reports waiting in the queue. |
| `{"sleep": ms}` | Waits. The maximum is 2000. |
| `{"need": COND}` | Fails unless `COND` holds. |
| `{"set": {"x": EXPR}}` | Sets variables. |
| `{"first": [[...], [...]]}` | Runs the first branch that goes through. `[]` always goes through. |
| `{"retry": n, "gap": ms, "do": [...]}` | Runs the block up to `n` times until it goes through. |
| `{"each": "x", "in": [..], "do": [...]}` | Runs the block for every item. A failed item does not stop the loop. |
| `{"any": "x", "in": [..], "do": [...]}` | Like `each`, but stops at the first item that goes through, and fails if none does. |
| `{"call": "name"}` | Runs a block from `subs`. |
| `{"emit": {...}}` | Reports a device (see below). |
| `{"stop": 1}` | Ends the run, as a success. |
| `{"fail": 1}` | Fails. |

Frames are hex bytes and `$var` bytes. They always start with the report ID, `00` when reports are not
numbered. A frame is padded with zeros to the collection's length.

A checksum `CK` is written into the frame at `at`: `{"t": "xor" | "sum" | "sum16le", "from": a, "to": b, "at": c}`.
`from` and `to` are inclusive frame offsets. With `"sub": k`, the checksum is `k - sum`. `sum16le` writes two
bytes.

### Values

- `EXPR` is one of:
  - a number;
  - `"$var"`;
  - a hex string such as `"1F"`;
  - `null`, which means unknown;
  - `{"b": off}`: a byte of the last reply;
  - `{"u16be": off}`, `{"u16le": off}`;
  - `{"len": 0}`: the reply's length;
  - the arithmetic operators `add`, `sub`, `mul`, `div`, `and`, `or`, `shr`, `shl`, `min` and `max`, each written
    `{"add": [a, b]}`;
  - `{"scale": [x, lo, hi]}`: `(x - lo) * 100 / (hi - lo)`, kept within 0..100;
  - `{"pick": [i, [t0, t1, ...]]}`: entry `i` of a table;
  - `{"curve": [name, mV]}`;
  - `{"or_else": [a, b]}`: the first value that is known;
  - `{"if": [COND, a, b]}`.
- `COND` is one of:
  - the comparisons `eq`, `ne`, `lt`, `le`, `gt` and `ge`, each written `{"eq": [a, b]}`;
  - `{"in": [x, [..]]}`;
  - `{"known": EXPR}`;
  - `{"len": n}`: the reply has at least `n` bytes;
  - `{"at": [off, "08 03 $x"]}`: the reply has these bytes from `off`;
  - `{"ck": CK}`: the reply's own checksum is right;
  - the combinators `all`, `any` and `not`;
  - `true` or `false`.

### Emit

```json
{"emit": {"slot": "kb", "kind": "keyboard", "percent": {"b": 2}, "charging": COND, "full": COND,
          "offline": COND, "asleep": COND, "state": "awake", "id": {"hex": [1, 4]}, "name": "..."}}
```

- `percent`: a value outside 0..100, or an unknown one, leaves the level unknown.
- `state`: `awake`, `asleep` (it keeps its last level for `grace`) or `offline` (switched off: it goes at once).
  Instead of `state`, `offline` and `asleep` can each be given as a condition.
- `slot`: tells apart the devices behind one receiver.
- `id`: replaces `VID:PID:slot` in the key. Use it for one key over cable and receiver, such as a unit ID.
- A run that emits nothing means "no answer". The devices the protocol reported before then keep their last
  level, as if asleep.

### Tests

```json
{"protocol": "f7", "feat": 65, "out": 65, "vars": {}, "replies": ["00 01 37 50 00 01 00"],
 "sent": ["00 F7"], "expect": [{"slot": "kb", "percent": 55}, {"slot": "ms", "state": "offline"}]}
```

- `replies` answer each `fget`, `iget` and `read` in turn.
- `sent` checks the first frames written.
- `expect` lists what the run must emit, in order. An emit's `charging` and `state` default to false and
  `awake`; its `percent` defaults to unknown.
