use std::collections::HashMap;

use mlua::prelude::*;
use rquickjs::{
    Array, ArrayBuffer, Atom, Ctx, Function, Object, Persistent, TypedArray, Value,
    atom::PredefinedAtom,
    object::{Filter, Property},
};

use crate::{
    context::JsContext,
    error::{JsError, catch},
    function::JsFunction,
};

/**
    The Luau representation of JavaScript `null`.
*/
#[derive(Debug, Clone, Copy)]
pub struct JsNull;

impl LuaUserData for JsNull {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, (): ()| Ok("js.Null"));
    }
}

/**
    The Luau representation of JavaScript `undefined`.
*/
#[derive(Debug, Clone, Copy)]
pub struct JsUndefined;

impl LuaUserData for JsUndefined {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, (): ()| Ok("js.Undefined"));
    }
}

#[derive(Clone)]
pub struct JsSentinels {
    null: LuaAnyUserData,
    undefined: LuaAnyUserData,
}

impl JsSentinels {
    pub fn new(lua: &Lua) -> LuaResult<Self> {
        Ok(Self {
            null: lua.create_userdata(JsNull)?,
            undefined: lua.create_userdata(JsUndefined)?,
        })
    }

    pub fn null(&self) -> LuaValue {
        LuaValue::UserData(self.null.clone())
    }

    pub fn undefined(&self) -> LuaValue {
        LuaValue::UserData(self.undefined.clone())
    }
}

pub fn sentinels(lua: &Lua) -> LuaResult<JsSentinels> {
    if let Some(sentinels) = lua.app_data_ref::<JsSentinels>() {
        return Ok(sentinels.clone());
    }
    let sentinels = JsSentinels::new(lua)?;
    lua.set_app_data(sentinels.clone());
    Ok(sentinels)
}

pub fn js_to_lua<'js>(
    ctx: &Ctx<'js>,
    lua: &Lua,
    context: &JsContext,
    value: Value<'js>,
) -> Result<LuaValue, JsError> {
    let mut values = js_to_lua_many(ctx, lua, context, vec![value])?;
    Ok(values.pop_front().expect("one converted value"))
}

pub fn js_to_lua_many<'js>(
    ctx: &Ctx<'js>,
    lua: &Lua,
    context: &JsContext,
    values: Vec<Value<'js>>,
) -> Result<LuaMultiValue, JsError> {
    let mut converter = JsToLua {
        lua,
        context,
        objects: HashMap::new(),
        items: 0,
    };
    values
        .into_iter()
        .map(|value| converter.convert(ctx, value, 0))
        .collect()
}

/** The deepest JavaScript value graph that conversion will walk. */
const MAX_CONVERSION_DEPTH: usize = 256;

/** The most entries conversion will materialize in one operation. */
const MAX_CONVERSION_ITEMS: usize = 1_000_000;

/** How often conversion rechecks the operation's limits. */
const CONVERSION_LIMIT_INTERVAL: usize = 4096;

struct JsToLua<'a, 'js> {
    lua: &'a Lua,
    context: &'a JsContext,
    objects: HashMap<Object<'js>, LuaValue>,
    items: usize,
}

impl<'js> JsToLua<'_, 'js> {
    fn convert(
        &mut self,
        ctx: &Ctx<'js>,
        value: Value<'js>,
        depth: usize,
    ) -> Result<LuaValue, JsError> {
        if depth > MAX_CONVERSION_DEPTH {
            return Err(JsError::new(format!(
                "JavaScript value is nested more than {MAX_CONVERSION_DEPTH} levels deep"
            )));
        }
        if value.is_undefined() {
            return sentinels(self.lua)
                .map(|sentinels| sentinels.undefined())
                .map_err(|error| JsError::new(error.to_string()));
        }
        if value.is_null() {
            return sentinels(self.lua)
                .map(|sentinels| sentinels.null())
                .map_err(|error| JsError::new(error.to_string()));
        }
        if let Some(value) = value.as_bool() {
            return Ok(LuaValue::Boolean(value));
        }
        if let Some(value) = value.as_number() {
            return Ok(LuaValue::Number(value));
        }
        if let Some(value) = value.as_string() {
            let value = catch(ctx, value.to_string())?;
            return self
                .lua
                .create_string(value)
                .map(LuaValue::String)
                .map_err(|error| JsError::new(error.to_string()));
        }
        if value.is_big_int() {
            return Err(unsupported("BigInt"));
        }
        if value.is_symbol() {
            return Err(unsupported("symbol"));
        }
        if value.is_proxy() {
            return Err(unsupported("Proxy"));
        }

        let object = value
            .clone()
            .into_object()
            .ok_or_else(|| unsupported(value.type_name()))?;

        if let Some(existing) = self.objects.get(&object) {
            return Ok(existing.clone());
        }

        if object.is_function() {
            let function = catch(ctx, Function::from_value(value))?;
            let persistent = Persistent::save(ctx, function);
            let function = self
                .lua
                .create_userdata(JsFunction::new(self.context.clone(), persistent))
                .map(LuaValue::UserData)
                .map_err(|error| JsError::new(error.to_string()))?;
            self.objects.insert(object, function.clone());
            return Ok(function);
        }

        if let Some(buffer) = ArrayBuffer::from_value(value.clone()) {
            let bytes = buffer
                .as_raw()
                .map(|bytes| unsafe { bytes.as_ref() }.to_vec())
                .ok_or_else(|| JsError::new("Cannot convert a detached JavaScript ArrayBuffer"))?;
            return self
                .lua
                .create_buffer(bytes)
                .map(LuaValue::Buffer)
                .map_err(|error| JsError::new(error.to_string()));
        }
        // QuickJS records a failed ArrayBuffer probe as a pending exception.
        let _ = ctx.catch();

        if let Ok(array) = TypedArray::<u8>::from_value(value.clone()) {
            let bytes = array
                .as_raw()
                .map(|bytes| unsafe { bytes.as_ref() }.to_vec())
                .ok_or_else(|| JsError::new("Cannot convert a detached JavaScript Uint8Array"))?;
            return self
                .lua
                .create_buffer(bytes)
                .map(LuaValue::Buffer)
                .map_err(|error| JsError::new(error.to_string()));
        }
        // A failed typed-array probe must not poison subsequent object conversion either.
        let _ = ctx.catch();

        if object.is_array() {
            return self.convert_array(ctx, object, depth);
        }

        self.convert_object(ctx, object, depth)
    }

    /// Counts one materialized entry and periodically enforces the operation's
    /// execution and timeout limits, which cannot reach this loop otherwise.
    fn charge(&mut self) -> Result<(), JsError> {
        self.items += 1;
        if self.items > MAX_CONVERSION_ITEMS {
            return Err(JsError::new(format!(
                "JavaScript value has more than {MAX_CONVERSION_ITEMS} entries"
            )));
        }
        if self.items % CONVERSION_LIMIT_INTERVAL == 0 {
            self.context.check_limits()?;
        }
        Ok(())
    }

    fn convert_array(
        &mut self,
        ctx: &Ctx<'js>,
        object: Object<'js>,
        depth: usize,
    ) -> Result<LuaValue, JsError> {
        reject_symbol_keys(ctx, &object)?;
        let array = object
            .clone()
            .into_array()
            .expect("the object was checked as an array");
        if array.len() > MAX_CONVERSION_ITEMS {
            return Err(JsError::new(format!(
                "JavaScript value has more than {MAX_CONVERSION_ITEMS} entries"
            )));
        }
        let table = self
            .lua
            .create_table()
            .map_err(|error| JsError::new(error.to_string()))?;
        self.objects.insert(object, LuaValue::Table(table.clone()));

        for index in 0..array.len() {
            self.charge()?;
            let value = catch(ctx, array.get::<Value>(index))?;
            let value = self.convert(ctx, value, depth + 1)?;
            table
                .raw_set(index + 1, value)
                .map_err(|error| JsError::new(error.to_string()))?;
        }
        Ok(LuaValue::Table(table))
    }

    fn convert_object(
        &mut self,
        ctx: &Ctx<'js>,
        object: Object<'js>,
        depth: usize,
    ) -> Result<LuaValue, JsError> {
        let ordinary_prototype = catch(ctx, Object::new(ctx.clone()))?.get_prototype();
        let prototype = object.get_prototype();
        let is_plain = match (prototype, ordinary_prototype) {
            (None, _) => true,
            (Some(prototype), Some(ordinary)) => prototype == ordinary,
            (Some(_), None) => false,
        };
        if !is_plain {
            return Err(JsError::new(
                "JavaScript value type 'object' is not supported; only plain objects are supported",
            ));
        }
        reject_symbol_keys(ctx, &object)?;

        let table = self
            .lua
            .create_table()
            .map_err(|error| JsError::new(error.to_string()))?;
        self.objects
            .insert(object.clone(), LuaValue::Table(table.clone()));
        for entry in object.props::<Atom, Value>() {
            let (key, value) = catch(ctx, entry)?;
            let key = catch(ctx, key.to_string())?;
            self.charge()?;
            let value = self.convert(ctx, value, depth + 1)?;
            table
                .set(key, value)
                .map_err(|error| JsError::new(error.to_string()))?;
        }
        Ok(LuaValue::Table(table))
    }
}

pub fn lua_to_js<'js>(
    ctx: &Ctx<'js>,
    values: impl IntoIterator<Item = LuaValue>,
) -> Result<Vec<Value<'js>>, JsError> {
    let mut converter = LuaToJs {
        objects: HashMap::new(),
    };
    values
        .into_iter()
        .map(|value| converter.convert(ctx, value))
        .collect()
}

pub fn lua_returns_to_js<'js>(
    ctx: &Ctx<'js>,
    values: LuaMultiValue,
) -> Result<Value<'js>, JsError> {
    let count = values.len();
    let values = lua_to_js(ctx, values)?;
    match count {
        0 => Ok(Value::new_undefined(ctx.clone())),
        1 => Ok(values.into_iter().next().expect("one converted value")),
        _ => {
            let array = catch(ctx, Array::new(ctx.clone()))?;
            for (index, value) in values.into_iter().enumerate() {
                catch(ctx, array.set(index, value))?;
            }
            Ok(array.into_value())
        }
    }
}

struct LuaToJs<'js> {
    objects: HashMap<usize, Object<'js>>,
}

impl<'js> LuaToJs<'js> {
    fn convert(&mut self, ctx: &Ctx<'js>, value: LuaValue) -> Result<Value<'js>, JsError> {
        match value {
            LuaValue::Nil => Ok(Value::new_null(ctx.clone())),
            LuaValue::Boolean(value) => Ok(Value::new_bool(ctx.clone(), value)),
            LuaValue::Integer(value) => Ok(Value::new_number(ctx.clone(), value as f64)),
            LuaValue::Number(value) => Ok(Value::new_number(ctx.clone(), value)),
            LuaValue::String(value) => {
                let value = value
                    .to_str()
                    .map_err(|error| JsError::new(error.to_string()))?;
                let string = catch(ctx, rquickjs::String::from_str(ctx.clone(), value.as_ref()))?;
                Ok(string.into_value())
            }
            LuaValue::Buffer(value) => {
                let bytes = value.to_vec();
                let buffer = catch(ctx, TypedArray::<u8>::new_copy(ctx.clone(), bytes))?;
                Ok(buffer.into_value())
            }
            LuaValue::Table(table) => self.convert_table(ctx, table),
            LuaValue::UserData(userdata) => {
                if userdata.borrow::<JsNull>().is_ok() {
                    return Ok(Value::new_null(ctx.clone()));
                }
                if userdata.borrow::<JsUndefined>().is_ok() {
                    return Ok(Value::new_undefined(ctx.clone()));
                }
                if let Ok(function) = userdata.borrow::<JsFunction>() {
                    return catch(ctx, function.persistent().restore(ctx))
                        .map(Function::into_value);
                }
                Err(JsError::new(
                    "Luau userdata is not supported; expected js.Null, js.Undefined, or an exported JavaScript function",
                ))
            }
            other => Err(JsError::new(format!(
                "Luau value type '{}' is not supported",
                other.type_name()
            ))),
        }
    }

    fn convert_table(&mut self, ctx: &Ctx<'js>, table: LuaTable) -> Result<Value<'js>, JsError> {
        if table.has_metatable() {
            return Err(JsError::new(
                "Luau tables with metatables are not supported",
            ));
        }
        let pointer = table.to_pointer() as usize;
        if let Some(existing) = self.objects.get(&pointer) {
            return Ok(existing.clone().into_value());
        }

        let mut length = 0usize;
        let mut numeric_keys = 0usize;
        let mut string_keys = 0usize;
        for pair in table.clone().pairs::<LuaValue, LuaValue>() {
            let (key, _) = pair.map_err(|error| JsError::new(error.to_string()))?;
            match key {
                LuaValue::Integer(index) if index > 0 => {
                    numeric_keys += 1;
                    length = length.max(index as usize);
                }
                LuaValue::String(_) => string_keys += 1,
                other => {
                    return Err(JsError::new(format!(
                        "Luau table keys of type '{}' are not supported",
                        other.type_name()
                    )));
                }
            }
        }

        if string_keys == 0 {
            if length != numeric_keys {
                return Err(JsError::new(
                    "Sparse or non-contiguous Luau array tables are not supported",
                ));
            }
            let array = catch(ctx, Array::new(ctx.clone()))?;
            self.objects.insert(pointer, array.clone().into_object());
            for pair in table.pairs::<usize, LuaValue>() {
                let (index, value) = pair.map_err(|error| JsError::new(error.to_string()))?;
                let value = self.convert(ctx, value)?;
                catch(ctx, array.set(index - 1, value))?;
            }
            Ok(array.into_value())
        } else if numeric_keys == 0 {
            let object = catch(ctx, Object::new(ctx.clone()))?;
            self.objects.insert(pointer, object.clone());
            for pair in table.pairs::<LuaString, LuaValue>() {
                let (key, value) = pair.map_err(|error| JsError::new(error.to_string()))?;
                let value = self.convert(ctx, value)?;
                let key = key
                    .to_str()
                    .map_err(|error| JsError::new(error.to_string()))?;
                let property = Property::from(value).enumerable().writable().configurable();
                catch(ctx, object.prop(key.as_ref(), property))?;
            }
            Ok(object.into_value())
        } else {
            Err(JsError::new(
                "Luau tables with mixed array and object keys are not supported",
            ))
        }
    }
}

fn reject_symbol_keys<'js>(ctx: &Ctx<'js>, object: &Object<'js>) -> Result<(), JsError> {
    let namespace_tag = Atom::from_predefined(ctx.clone(), PredefinedAtom::SymbolToStringTag);
    for key in object.own_keys::<Atom>(Filter::new().symbol()) {
        let key = catch(ctx, key)?;
        if key != namespace_tag {
            return Err(JsError::new(
                "JavaScript objects with symbol keys other than Symbol.toStringTag are not supported",
            ));
        }
    }
    Ok(())
}

fn unsupported(kind: &str) -> JsError {
    JsError::new(format!("JavaScript value type '{kind}' is not supported"))
}
