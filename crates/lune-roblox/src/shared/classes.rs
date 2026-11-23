use mlua::prelude::*;

use rbx_dom_weak::types::Variant as DomValue;

use crate::instance::{Instance, base::ensure_not_destroyed, instance_to_lua};

use super::instance::class_is_a;

/**
    Checks that a class-restricted member is used on a live instance of its class.

    Class members read the instance's document, so a destroyed instance gets an
    ordinary Luau error here instead of reaching document lookups that expect it to
    still exist.
*/
fn ensure_class_member(
    this: &Instance,
    class_name: &'static str,
    member_name: &'static str,
) -> LuaResult<()> {
    if class_is_a(this.get_class_name(), class_name).unwrap_or(false) {
        ensure_not_destroyed(this)
    } else {
        Err(LuaError::RuntimeError(format!(
            "{member_name} is not a valid member of {class_name}",
        )))
    }
}

pub(crate) fn add_class_restricted_getter<F: LuaUserDataFields<Instance>, R, G>(
    fields: &mut F,
    class_name: &'static str,
    field_name: &'static str,
    field_getter: G,
) where
    R: IntoLua,
    G: 'static + Fn(&Lua, &Instance) -> LuaResult<R>,
{
    fields.add_field_method_get(field_name, move |lua, this| {
        ensure_class_member(this, class_name, field_name)?;
        field_getter(lua, this)
    });
}

#[allow(dead_code)]
pub(crate) fn add_class_restricted_setter<F: LuaUserDataFields<Instance>, A, G>(
    fields: &mut F,
    class_name: &'static str,
    field_name: &'static str,
    field_getter: G,
) where
    A: FromLua,
    G: 'static + Fn(&Lua, &Instance, A) -> LuaResult<()>,
{
    fields.add_field_method_set(field_name, move |lua, this, value| {
        ensure_class_member(this, class_name, field_name)?;
        field_getter(lua, this, value)
    });
}

pub(crate) fn add_class_restricted_method<M: LuaUserDataMethods<Instance>, A, R, F>(
    methods: &mut M,
    class_name: &'static str,
    method_name: &'static str,
    method: F,
) where
    A: FromLuaMulti,
    R: IntoLuaMulti,
    F: 'static + Fn(&Lua, &Instance, A) -> LuaResult<R>,
{
    methods.add_method(method_name, move |lua, this, args| {
        ensure_class_member(this, class_name, method_name)?;
        method(lua, this, args)
    });
}

pub(crate) fn add_class_restricted_method_mut<M: LuaUserDataMethods<Instance>, A, R, F>(
    methods: &mut M,
    class_name: &'static str,
    method_name: &'static str,
    method: F,
) where
    A: FromLuaMulti,
    R: IntoLuaMulti,
    F: 'static + Fn(&Lua, &mut Instance, A) -> LuaResult<R>,
{
    methods.add_method_mut(method_name, move |lua, this, args| {
        ensure_class_member(this, class_name, method_name)?;
        method(lua, this, args)
    });
}

/**
    Gets or creates the instance child with the given reference prop name and class name.

    Note that the class name here must be an exact match, it is not checked using `IsA`.

    The instance may be in one of several states but this function will guarantee that the
    property reference is correct and that the instance exists after it has been called:

    1. Instance exists as property ref - just return it
    2. Instance exists under workspace but not as a property ref - save it and return it
    3. Instance does not exist - create it, save it, then return it
*/
pub(crate) fn get_or_create_property_ref_instance(
    lua: &Lua,
    this: &Instance,
    prop_name: &'static str,
    class_name: &'static str,
) -> LuaResult<LuaValue> {
    let inst = if let Some(DomValue::Ref(inst_ref)) = this.get_property(prop_name)
        && let Some(inst) = Instance::new_opt(this.dom_id, inst_ref)
    {
        inst
    } else if let Some(inst) = this.find_child(|child| child.class == class_name) {
        this.set_property(prop_name, DomValue::Ref(inst.dom_ref));
        inst
    } else {
        // Create the child directly inside this instance's dom so that
        // parenting it below stays within the dom (no cross-dom transfer).
        let inst = Instance::new_in_dom(this.dom_id, class_name);
        inst.set_parent(Some(*this));
        this.set_property(prop_name, DomValue::Ref(inst.dom_ref));
        inst
    };
    instance_to_lua(lua, inst)
}
