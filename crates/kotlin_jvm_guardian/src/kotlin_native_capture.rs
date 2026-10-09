use jni::{
    JNIEnv,
    objects::{GlobalRef, JClass, JObject, JValue},
    sys::{jboolean, jlong, jlongArray},
};
#[cfg(test)]
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};

const FRAME_BYTES: usize = 16 * 1024 * 1024;
const VALUE_NODES: usize = 131_072;
const CHUNK_BYTES: usize = 65_536;
const MAXIMUM_DEPTH: usize = 128;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy)]
struct Limits {
    bytes: usize,
    nodes: usize,
    scalar: usize,
}

impl Limits {
    fn new(bytes: jlong, nodes: jlong, scalar: jlong) -> Result<Self> {
        let positive = |value, ceiling| {
            usize::try_from(value)
                .ok()
                .filter(|value| *value > 0 && *value <= ceiling)
                .ok_or_else(|| "Invalid Rust-issued producer limits".to_owned())
        };
        Ok(Self {
            bytes: positive(bytes, FRAME_BYTES)?,
            nodes: positive(nodes, VALUE_NODES)?,
            scalar: positive(scalar, FRAME_BYTES)?,
        })
    }
}

#[derive(Default)]
struct Output {
    bytes: usize,
    nodes: usize,
    chunks: Vec<Vec<u8>>,
}

impl Output {
    fn emit(&mut self, byte: u8, limits: Limits, retain: bool) -> Result<()> {
        if self.bytes >= limits.bytes {
            return Err("Producer serialized-byte budget exceeded".into());
        }
        if retain {
            if self
                .chunks
                .last()
                .is_none_or(|chunk| chunk.len() == CHUNK_BYTES)
            {
                let capacity = CHUNK_BYTES.min(limits.bytes - self.bytes);
                let mut chunk = Vec::new();
                chunk
                    .try_reserve_exact(capacity)
                    .map_err(|error| error.to_string())?;
                self.chunks
                    .try_reserve(1)
                    .map_err(|error| error.to_string())?;
                self.chunks.push(chunk);
            }
            self.chunks
                .last_mut()
                .ok_or("Missing admitted output chunk")?
                .push(byte);
        }
        self.bytes += 1;
        Ok(())
    }

    fn value(&mut self, limits: Limits, depth: usize) -> Result<()> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or("Producer value-node budget exceeded")?;
        if self.nodes > limits.nodes {
            return Err("Producer value-node budget exceeded".into());
        }
        if depth >= MAXIMUM_DEPTH {
            return Err("Producer JSON depth budget exceeded".into());
        }
        Ok(())
    }
}

fn update_usage(
    bytes: jlong,
    nodes: jlong,
    removed_bytes: jlong,
    removed_nodes: jlong,
    added_bytes: jlong,
    added_nodes: jlong,
    separator: bool,
    maximum_bytes: jlong,
    maximum_nodes: jlong,
) -> Result<[jlong; 2]> {
    let limits = Limits::new(maximum_bytes, maximum_nodes, FRAME_BYTES as jlong)?;
    if [
        bytes,
        nodes,
        removed_bytes,
        removed_nodes,
        added_bytes,
        added_nodes,
    ]
    .iter()
    .any(|value| *value < 0)
    {
        return Err("Producer cumulative observation budget exceeded".into());
    }
    let bytes = bytes
        .checked_sub(removed_bytes)
        .filter(|value| *value >= 0)
        .and_then(|value| value.checked_add(added_bytes))
        .and_then(|value| value.checked_add(jlong::from(separator)))
        .filter(|value| *value <= limits.bytes as jlong)
        .ok_or("Producer cumulative observation budget exceeded")?;
    let nodes = nodes
        .checked_sub(removed_nodes)
        .filter(|value| *value >= 0)
        .and_then(|value| value.checked_add(added_nodes))
        .filter(|value| *value <= limits.nodes as jlong)
        .ok_or("Producer cumulative observation budget exceeded")?;
    Ok([bytes, nodes])
}

fn encode_utf16_unit(
    character: u16,
    previous_high: &mut bool,
    scalar_bytes: &mut usize,
    maximum_scalar_bytes: usize,
    mut emit: impl FnMut(u8) -> Result<()>,
) -> Result<()> {
    let width = if ((0xdc00..=0xdfff).contains(&character) && *previous_high) || character < 128 {
        1
    } else if character < 2048 {
        2
    } else {
        3
    };
    *scalar_bytes = scalar_bytes
        .checked_add(width)
        .ok_or("Producer scalar budget exceeded")?;
    *previous_high = (0xd800..=0xdbff).contains(&character);
    if *scalar_bytes > maximum_scalar_bytes {
        return Err("Producer scalar budget exceeded".into());
    }
    let escaped: Option<&[u8]> = match character {
        34 => Some(b"\\\""),
        92 => Some(b"\\\\"),
        8 => Some(b"\\b"),
        9 => Some(b"\\t"),
        10 => Some(b"\\n"),
        12 => Some(b"\\f"),
        13 => Some(b"\\r"),
        _ => None,
    };
    if let Some(escaped) = escaped {
        for byte in escaped {
            emit(*byte)?;
        }
    } else if character < 32 || character > 127 {
        emit(b'\\')?;
        emit(b'u')?;
        for shift in [12, 8, 4, 0] {
            let digit = ((character >> shift) & 15) as u8;
            emit(if digit < 10 {
                b'0' + digit
            } else {
                b'a' + digit - 10
            })?;
        }
    } else {
        emit(character as u8)?;
    }
    Ok(())
}

struct Encoder<'environment, 'local> {
    environment: &'environment mut JNIEnv<'local>,
    monitor: &'environment JObject<'local>,
    output: &'environment mut Output,
    limits: Limits,
    retain: bool,
    active: Vec<GlobalRef>,
}

impl Encoder<'_, '_> {
    fn health(&mut self) -> Result<()> {
        let result = self
            .environment
            .call_method(self.monitor, "call", "()Ljava/lang/Object;", &[])
            .map_err(|error| error.to_string())?
            .l()
            .map_err(|error| error.to_string())?;
        self.environment
            .delete_local_ref(result)
            .map_err(|error| error.to_string())
    }

    fn emit(&mut self, byte: u8) -> Result<()> {
        self.output.emit(byte, self.limits, self.retain)?;
        if self.output.bytes & 4095 == 0 {
            self.health()?;
        }
        Ok(())
    }

    fn ascii(&mut self, bytes: &[u8]) -> Result<()> {
        for byte in bytes {
            self.emit(*byte)?;
        }
        Ok(())
    }

    fn string(&mut self, value: &JObject<'_>) -> Result<()> {
        // Reading fixed UTF-16 regions avoids copying a giant JVM String before
        // admission. JNI modified UTF-8 would also change NUL/surrogate semantics.
        let interface = self.environment.get_native_interface();
        // SAFETY: JNIEnv is live on this native call's JVM thread. The caller has
        // established java.lang.String identity before any string operation.
        let functions = unsafe { &**interface };
        let length_function = functions
            .GetStringLength
            .ok_or("JVM lacks GetStringLength")?;
        let region_function = functions
            .GetStringRegion
            .ok_or("JVM lacks GetStringRegion")?;
        let string = value.as_raw();
        // SAFETY: value is a live String local reference and interface is its JNI environment.
        let length = unsafe { length_function(interface, string) };
        let length = usize::try_from(length).map_err(|error| error.to_string())?;
        if length > self.limits.scalar || length > self.limits.bytes {
            return Err("Producer scalar budget exceeded".into());
        }
        self.emit(b'"')?;
        let mut region = [0_u16; 1024];
        let mut position = 0;
        let mut scalar_bytes = 0_usize;
        let mut previous_high = false;
        while position < length {
            self.health()?;
            let count = region.len().min(length - position);
            // SAFETY: the fixed buffer has count UTF-16 slots, and this region
            // lies within the immutable String length checked above.
            unsafe {
                region_function(
                    interface,
                    string,
                    position as i32,
                    count as i32,
                    region.as_mut_ptr(),
                )
            };
            if self
                .environment
                .exception_check()
                .map_err(|error| error.to_string())?
            {
                return Err("JVM string region read failed".into());
            }
            for character in region.iter().take(count).copied() {
                let scalar_limit = self.limits.scalar;
                encode_utf16_unit(
                    character,
                    &mut previous_high,
                    &mut scalar_bytes,
                    scalar_limit,
                    |byte| self.emit(byte),
                )?;
            }
            position += count;
        }
        self.emit(b'"')
    }

    fn is(&mut self, value: &JObject<'_>, class: &str) -> Result<bool> {
        self.environment
            .is_instance_of(value, class)
            .map_err(|error| error.to_string())
    }

    fn value(&mut self, value: &JObject<'_>, depth: usize) -> Result<()> {
        self.health()?;
        self.output.value(self.limits, depth)?;
        if value.is_null() {
            return self.ascii(b"null");
        }
        if self.is(value, "java/lang/String")? {
            return self.string(value);
        }
        if self.is(value, "java/lang/Boolean")? {
            let boolean = self
                .environment
                .call_method(value, "booleanValue", "()Z", &[])
                .map_err(|error| error.to_string())?
                .z()
                .map_err(|error| error.to_string())?;
            return self.ascii(if boolean { b"true" } else { b"false" });
        }
        if self.is(value, "java/lang/Byte")?
            || self.is(value, "java/lang/Short")?
            || self.is(value, "java/lang/Integer")?
            || self.is(value, "java/lang/Long")?
        {
            let number = self
                .environment
                .call_method(value, "longValue", "()J", &[])
                .map_err(|error| error.to_string())?
                .j()
                .map_err(|error| error.to_string())?;
            return self.ascii(number.to_string().as_bytes());
        }
        let map = self.is(value, "java/util/Map")?;
        if !map && !self.is(value, "java/lang/Iterable")? {
            return Err("Unsupported raw observation JSON value".into());
        }
        for active in &self.active {
            if self
                .environment
                .is_same_object(active.as_obj(), value)
                .map_err(|error| error.to_string())?
            {
                return Err("Cyclic raw observation JSON value".into());
            }
        }
        self.active.push(
            self.environment
                .new_global_ref(value)
                .map_err(|error| error.to_string())?,
        );
        let result = self.container(value, depth, map);
        drop(self.active.pop());
        result
    }

    fn container(&mut self, value: &JObject<'_>, depth: usize, map: bool) -> Result<()> {
        let source = if map {
            self.environment
                .call_method(value, "entrySet", "()Ljava/util/Set;", &[])
                .map_err(|error| error.to_string())?
                .l()
                .map_err(|error| error.to_string())?
        } else {
            self.environment
                .new_local_ref(value)
                .map_err(|error| error.to_string())?
        };
        let source = self.environment.auto_local(source);
        let iterator = self
            .environment
            .call_method(&source, "iterator", "()Ljava/util/Iterator;", &[])
            .map_err(|error| error.to_string())?
            .l()
            .map_err(|error| error.to_string())?;
        let iterator = self.environment.auto_local(iterator);
        self.emit(if map { b'{' } else { b'[' })?;
        let mut first = true;
        loop {
            self.health()?;
            let more = self
                .environment
                .call_method(&iterator, "hasNext", "()Z", &[])
                .map_err(|error| error.to_string())?
                .z()
                .map_err(|error| error.to_string())?;
            if !more {
                break;
            }
            if self.output.nodes >= self.limits.nodes {
                return Err("Producer value-node budget exceeded".into());
            }
            let element = self
                .environment
                .call_method(&iterator, "next", "()Ljava/lang/Object;", &[])
                .map_err(|error| error.to_string())?
                .l()
                .map_err(|error| error.to_string())?;
            let element = self.environment.auto_local(element);
            if !first {
                self.emit(b',')?;
            }
            first = false;
            if map {
                let key = self
                    .environment
                    .call_method(&element, "getKey", "()Ljava/lang/Object;", &[])
                    .map_err(|error| error.to_string())?
                    .l()
                    .map_err(|error| error.to_string())?;
                let key = self.environment.auto_local(key);
                if !self.is(&key, "java/lang/String")? {
                    return Err("Raw observation key is not a String".into());
                }
                self.string(&key)?;
                self.emit(b':')?;
                let child = self
                    .environment
                    .call_method(&element, "getValue", "()Ljava/lang/Object;", &[])
                    .map_err(|error| error.to_string())?
                    .l()
                    .map_err(|error| error.to_string())?;
                let child = self.environment.auto_local(child);
                self.value(&child, depth + 1)?;
            } else {
                self.value(&element, depth + 1)?;
            }
        }
        self.emit(if map { b'}' } else { b']' })
    }
}

fn publish_state(
    environment: &mut JNIEnv<'_>,
    receiver: &JObject<'_>,
    output: &Output,
) -> Result<()> {
    environment
        .set_field(receiver, "bytes", "J", JValue::Long(output.bytes as jlong))
        .map_err(|error| error.to_string())?;
    environment
        .set_field(receiver, "nodes", "J", JValue::Long(output.nodes as jlong))
        .map_err(|error| error.to_string())?;
    let chunks = environment
        .new_object("java/util/ArrayList", "()V", &[])
        .map_err(|error| error.to_string())?;
    let chunks = environment.auto_local(chunks);
    for chunk in &output.chunks {
        let bytes = environment
            .byte_array_from_slice(chunk)
            .map_err(|error| error.to_string())?;
        let bytes = environment.auto_local(bytes);
        environment
            .call_method(
                &chunks,
                "add",
                "(Ljava/lang/Object;)Z",
                &[JValue::Object(bytes.as_ref())],
            )
            .map_err(|error| error.to_string())?;
    }
    environment
        .set_field(
            receiver,
            "chunks",
            "Ljava/lang/Object;",
            JValue::Object(chunks.as_ref()),
        )
        .map_err(|error| error.to_string())
}

fn fail(environment: &mut JNIEnv<'_>, error: &str) {
    match environment.exception_check() {
        Ok(true) => {}
        Ok(false) => {
            if let Err(error) = environment.throw_new("java/io/IOException", error) {
                eprintln!("Unable to report Rust producer failure to JVM: {error}");
            }
        }
        Err(error) => eprintln!("Unable to inspect JVM producer failure: {error}"),
    }
}

/// # Safety
/// Called only by the JVM with live local references and its current thread's JNI environment.
#[unsafe(export_name = "Java_KodaKotlinBoundedJson_encode")]
pub unsafe extern "system" fn encode_native<'local>(
    mut environment: JNIEnv<'local>,
    receiver: JObject<'local>,
    value: JObject<'local>,
    bytes: jlong,
    nodes: jlong,
    scalar: jlong,
    monitor: JObject<'local>,
    retain: jboolean,
) {
    let mut output = Output::default();
    let operation = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
        let limits = Limits::new(bytes, nodes, scalar)?;
        let result = Encoder {
            environment: &mut environment,
            monitor: &monitor,
            output: &mut output,
            limits,
            retain: retain != 0,
            active: Vec::new(),
        }
        .value(&value, 0);
        // A health/iterator exception is preserved verbatim across publishing the
        // bounded diagnostic counters needed by the unchanged JVM probes.
        let exception = if environment
            .exception_check()
            .map_err(|error| error.to_string())?
        {
            let exception = environment
                .exception_occurred()
                .map_err(|error| error.to_string())?;
            environment
                .exception_clear()
                .map_err(|error| error.to_string())?;
            Some(exception)
        } else {
            None
        };
        let publication = publish_state(&mut environment, &receiver, &output);
        if let Some(exception) = exception {
            if environment
                .exception_check()
                .map_err(|error| error.to_string())?
            {
                let secondary = environment
                    .exception_occurred()
                    .map_err(|error| error.to_string())?;
                environment
                    .exception_clear()
                    .map_err(|error| error.to_string())?;
                if let Err(error) = environment.call_method(
                    &exception,
                    "addSuppressed",
                    "(Ljava/lang/Throwable;)V",
                    &[JValue::Object(secondary.as_ref())],
                ) {
                    eprintln!("Unable to attach secondary producer publication failure: {error}");
                    environment
                        .exception_clear()
                        .map_err(|error| error.to_string())?;
                }
            }
            environment
                .throw(exception)
                .map_err(|error| error.to_string())?;
        }
        result.and(publication)
    }));
    match operation {
        Ok(Ok(())) => {}
        Ok(Err(error)) => fail(&mut environment, &error),
        Err(_) => fail(&mut environment, "Rust producer encoder panicked"),
    }
}

/// # Safety
/// Called by the JVM with its current thread's JNI environment.
#[unsafe(export_name = "Java_KodaKotlinBoundedJson_updateUsage")]
pub unsafe extern "system" fn update_usage_native(
    mut environment: JNIEnv<'_>,
    _class: JClass<'_>,
    bytes: jlong,
    nodes: jlong,
    removed_bytes: jlong,
    removed_nodes: jlong,
    added_bytes: jlong,
    added_nodes: jlong,
    separator: jboolean,
    maximum_bytes: jlong,
    maximum_nodes: jlong,
) -> jlongArray {
    let operation = catch_unwind(AssertUnwindSafe(|| -> Result<jlongArray> {
        let usage = update_usage(
            bytes,
            nodes,
            removed_bytes,
            removed_nodes,
            added_bytes,
            added_nodes,
            separator != 0,
            maximum_bytes,
            maximum_nodes,
        )?;
        let result = environment
            .new_long_array(2)
            .map_err(|error| error.to_string())?;
        environment
            .set_long_array_region(&result, 0, &usage)
            .map_err(|error| error.to_string())?;
        Ok(result.into_raw())
    }));
    match operation {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            fail(&mut environment, &error);
            std::ptr::null_mut()
        }
        Err(_) => {
            fail(&mut environment, "Rust producer accounting panicked");
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// Called by the JVM with its current thread's JNI environment.
#[unsafe(export_name = "Java_KodaKotlinBoundedJson_checkNext")]
pub unsafe extern "system" fn check_next_native(
    mut environment: JNIEnv<'_>,
    _class: JClass<'_>,
    nodes: jlong,
    maximum_nodes: jlong,
) {
    if maximum_nodes <= 0
        || maximum_nodes > VALUE_NODES as jlong
        || nodes < 0
        || nodes >= maximum_nodes
    {
        fail(
            &mut environment,
            "Producer iterable value-node budget exceeded",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_chunks_never_allocate_past_escaped_byte_boundary() -> io::Result<()> {
        let limits = Limits::new(10_000, 100, 10_000).map_err(io::Error::other)?;
        let mut output = Output::default();
        for _ in 0..10_000 {
            output.emit(b'x', limits, true).map_err(io::Error::other)?;
        }
        assert!(output.emit(b'x', limits, true).is_err());
        assert_eq!(output.bytes, 10_000);
        assert_eq!(output.chunks.iter().map(Vec::len).sum::<usize>(), 10_000);
        assert!(output.chunks.iter().map(Vec::capacity).sum::<usize>() <= limits.bytes);
        Ok(())
    }

    #[test]
    fn exact_value_node_and_depth_limits_are_rejected_at_first_excess() -> io::Result<()> {
        let limits = Limits::new(100, 2, 100).map_err(io::Error::other)?;
        let mut output = Output::default();
        output.value(limits, 0).map_err(io::Error::other)?;
        output.value(limits, 127).map_err(io::Error::other)?;
        assert!(output.value(limits, 0).is_err());
        let mut output = Output::default();
        assert!(output.value(limits, 128).is_err());
        Ok(())
    }

    #[test]
    fn accounting_replacements_preserve_exact_totals_and_reject_underflow() -> io::Result<()> {
        assert_eq!(
            update_usage(20, 4, 5, 1, 7, 2, true, 23, 5).map_err(io::Error::other)?,
            [23, 5]
        );
        assert!(update_usage(20, 4, 5, 1, 7, 2, true, 22, 5).is_err());
        assert!(update_usage(20, 4, 5, 1, 7, 2, true, 23, 4).is_err());
        assert!(update_usage(2, 1, 3, 0, 1, 0, false, 23, 5).is_err());
        assert!(update_usage(jlong::MAX, 1, 0, 0, 1, 0, false, 23, 5).is_err());
        assert!(update_usage(0, 0, 0, 0, -1, 0, false, 23, 5).is_err());
        Ok(())
    }
    #[test]
    fn unicode_and_controls_match_json_without_full_string_copy() -> io::Result<()> {
        let text = "quote:\" slash:\\ control:\0 euro:\u{20ac} emoji:\u{1f600}";
        let limits = Limits::new(4096, 100, text.len() as jlong).map_err(io::Error::other)?;
        let mut output = Output::default();
        let mut previous_high = false;
        let mut scalar_bytes = 0;
        output.emit(b'"', limits, true).map_err(io::Error::other)?;
        for character in text.encode_utf16() {
            encode_utf16_unit(
                character,
                &mut previous_high,
                &mut scalar_bytes,
                limits.scalar,
                |byte| output.emit(byte, limits, true),
            )
            .map_err(io::Error::other)?;
        }
        output.emit(b'"', limits, true).map_err(io::Error::other)?;
        assert_eq!(scalar_bytes, text.len());
        let actual: Vec<u8> = output.chunks.into_iter().flatten().collect();
        assert_eq!(
            actual,
            b"\"quote:\\\" slash:\\\\ control:\\u0000 euro:\\u20ac emoji:\\ud83d\\ude00\""
        );
        Ok(())
    }

    #[test]
    fn surrogate_pair_across_region_boundary_counts_four_utf8_bytes() -> io::Result<()> {
        let limits = Limits::new(2048, 100, 1027).map_err(io::Error::other)?;
        let mut output = Output::default();
        let mut previous_high = false;
        let mut scalar_bytes = 0;
        for _ in 0..1023 {
            encode_utf16_unit(
                b'x' as u16,
                &mut previous_high,
                &mut scalar_bytes,
                limits.scalar,
                |byte| output.emit(byte, limits, true),
            )
            .map_err(io::Error::other)?;
        }
        encode_utf16_unit(
            0xd83d,
            &mut previous_high,
            &mut scalar_bytes,
            limits.scalar,
            |byte| output.emit(byte, limits, true),
        )
        .map_err(io::Error::other)?;
        encode_utf16_unit(
            0xde00,
            &mut previous_high,
            &mut scalar_bytes,
            limits.scalar,
            |byte| output.emit(byte, limits, true),
        )
        .map_err(io::Error::other)?;
        assert_eq!(scalar_bytes, 1027);
        assert!(
            encode_utf16_unit(
                b'x' as u16,
                &mut previous_high,
                &mut scalar_bytes,
                limits.scalar,
                |byte| output.emit(byte, limits, true)
            )
            .is_err()
        );
        assert_eq!(output.bytes, 1035);
        Ok(())
    }

    #[test]
    fn escaped_output_rejects_before_first_over_budget_byte() -> io::Result<()> {
        let limits = Limits::new(10_000, 100, 10_000).map_err(io::Error::other)?;
        let mut output = Output::default();
        let mut previous_high = false;
        let mut scalar_bytes = 0;
        let mut rejected = false;
        for _ in 0..5000 {
            if encode_utf16_unit(
                0,
                &mut previous_high,
                &mut scalar_bytes,
                limits.scalar,
                |byte| output.emit(byte, limits, true),
            )
            .is_err()
            {
                rejected = true;
                break;
            }
        }
        assert!(rejected);
        assert_eq!(output.bytes, 10_000);
        assert!(output.chunks.iter().map(Vec::capacity).sum::<usize>() <= limits.bytes);
        Ok(())
    }
}
