use crate::ast::{Component, Element, PropertyKind, Value};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use std::collections::{HashMap, HashSet};
use syn::LitStr;

#[derive(Clone, Debug)]
pub struct GenerationError {
    pub message: String,
    pub offset: usize,
}

impl GenerationError {
    fn new(offset: usize, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            offset,
        }
    }

    #[cfg(test)]
    fn contains(&self, needle: &str) -> bool {
        self.message.contains(needle)
    }
}

impl std::fmt::Display for GenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

type GenerationResult<T> = Result<T, GenerationError>;

fn at<T>(offset: usize, result: Result<T, String>) -> GenerationResult<T> {
    result.map_err(|message| GenerationError::new(offset, message))
}

pub fn component(component: &Component, source_path: &LitStr) -> GenerationResult<TokenStream> {
    let component_name = at(component.offset, rust_ident(&component.name))?;
    validate(component)?;

    let mut property_types = HashMap::new();
    let mut fields = Vec::new();
    let mut initializers = Vec::new();
    let mut constructors = Vec::new();
    let mut methods = Vec::new();
    for property in &component.properties {
        let name = at(property.offset, rust_ident(&property.name))?;
        let setter = format_ident!("set_{}", normalized(&property.name));
        let property_handle = format_ident!("{}_property", normalized(&property.name));
        let ty = rust_type(property.kind);
        let initial = at(property.offset, literal(&property.initial, property.kind))?;
        property_types.insert(property.name.clone(), property.kind);
        fields.push(quote!(#name: ::slint_dom::Property<#ty>));
        initializers.push(quote!(let #name = ::slint_dom::Property::new(#initial);));
        constructors.push(quote!(#name));
        methods.push(quote! {
            pub fn #name(&self) -> #ty { self.#name.get() }
            pub fn #setter(&self, value: #ty) { self.#name.set(value); }
            pub fn #property_handle(&self) -> ::slint_dom::Property<#ty> { self.#name.clone() }
        });
    }

    let mut callback_names = HashSet::new();
    for callback in &component.callbacks {
        let name = at(callback.offset, rust_ident(&callback.name))?;
        let on_name = format_ident!("on_{}", normalized(&callback.name));
        callback_names.insert(callback.name.clone());
        fields.push(quote!(#name: ::slint_dom::Callback));
        initializers.push(quote!(let #name = ::slint_dom::Callback::default();));
        constructors.push(quote!(#name));
        methods.push(quote! {
            pub fn #on_name(&self, handler: impl FnMut() + 'static) { self.#name.set(handler); }
        });
    }

    let mut ids = Vec::new();
    collect_ids(&component.children, &mut ids);
    for (id, offset) in &ids {
        let name = at(*offset, rust_ident(id))?;
        fields.push(quote!(#name: ::slint_dom::__private::Element));
        constructors.push(quote!(#name));
        methods.push(quote! {
            pub fn #name(&self) -> &::slint_dom::__private::Element { &self.#name }
        });
    }

    let mut sequence = 0;
    let nodes = emit_nodes(
        &component.children,
        quote!(root),
        &property_types,
        &callback_names,
        &mut sequence,
    )?;
    let root_tag = component.root_tag;
    let title = component
        .title
        .as_ref()
        .map(|title| quote!(dom.set_title(#title);));

    Ok(quote! {
        const _: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", #source_path));

        pub struct #component_name {
            root: ::slint_dom::__private::Element,
            #(#fields,)*
            _events: ::std::vec::Vec<::slint_dom::EventBinding>,
            _subscriptions: ::std::vec::Vec<::slint_dom::Subscription>,
        }

        impl #component_name {
            pub fn mount(parent: &::slint_dom::__private::Element) -> Result<Self, ::slint_dom::__private::JsValue> {
                let dom = ::slint_dom::DomBuilder::from_browser()?;
                dom.install_default_style()?;
                #title
                #(#initializers)*
                let mut events = ::std::vec::Vec::new();
                let mut subscriptions = ::std::vec::Vec::new();
                let root = dom.element(#root_tag, "sd-component")?;
                #nodes
                dom.append(parent, &root)?;
                Ok(Self { root, #(#constructors,)* _events: events, _subscriptions: subscriptions })
            }

            pub fn mount_to_body() -> Result<Self, ::slint_dom::__private::JsValue> {
                let dom = ::slint_dom::DomBuilder::from_browser()?;
                let body = dom.body()?;
                Self::mount(&body)
            }

            pub fn root(&self) -> &::slint_dom::__private::Element { &self.root }
            pub fn unmount(self) { self.root.remove(); }
            #(#methods)*
        }
    })
}

fn emit_nodes(
    nodes: &[Element],
    parent: TokenStream,
    properties: &HashMap<String, PropertyKind>,
    callbacks: &HashSet<String>,
    sequence: &mut usize,
) -> GenerationResult<TokenStream> {
    let mut output = TokenStream::new();
    for node in nodes {
        let index = *sequence;
        *sequence += 1;
        let variable = node
            .id
            .as_ref()
            .map(|id| rust_ident(id))
            .transpose()
            .map_err(|message| GenerationError::new(node.offset, message))?
            .unwrap_or_else(|| format_ident!("__node_{index}"));
        let spec = at(node.offset, widget(&node.kind))?;
        if matches!(spec.tag, "input" | "img") && !node.children.is_empty() {
            return Err(GenerationError::new(
                node.offset,
                format!("void element `{}` cannot contain children", node.kind),
            ));
        }
        let mut seen_properties = HashSet::new();
        let mut seen_events = HashSet::new();
        let mut setup = TokenStream::new();
        if let Some(input_type) = spec.input_type {
            setup.extend(quote!(dom.attribute(&#variable, "type", #input_type)?;));
        }
        // HTML range inputs default to whole-number steps.  Slint's float
        // properties must retain their fractional values unless the UI
        // explicitly chooses a step size.
        if node.kind == "Slider"
            && !node
                .properties
                .iter()
                .any(|property| property.name == "step")
        {
            setup.extend(quote!(dom.attribute(&#variable, "step", "any")?;));
        }

        // Browsers clamp a range input's value to its current bounds, so a
        // slider's value is applied after `minimum`, `maximum`, and `step`
        // regardless of their order in the source.
        let mut deferred = TokenStream::new();
        for property in &node.properties {
            let name = &property.name;
            let value = &property.value;
            if !seen_properties.insert(name.as_str()) {
                return Err(GenerationError::new(
                    property.offset,
                    format!(
                        "property `{name}` is assigned more than once on `{}`",
                        node.kind
                    ),
                ));
            }
            if !spec.properties.contains(&name.as_str())
                && !COMMON_PROPERTIES.contains(&name.as_str())
            {
                return Err(GenerationError::new(
                    property.offset,
                    format!("property `{name}` is not supported on `{}`", node.kind),
                ));
            }
            let tokens = at(
                property.offset,
                emit_property(&variable, &node.kind, name, value, properties),
            )?;
            if node.kind == "Slider" && name == "value" {
                deferred.extend(tokens);
            } else {
                setup.extend(tokens);
            }
        }
        setup.extend(deferred);
        for handler in &node.handlers {
            if !seen_events.insert(handler.event.as_str()) {
                return Err(GenerationError::new(
                    handler.offset,
                    format!(
                        "event `{}` is handled more than once on `{}`",
                        handler.event, node.kind
                    ),
                ));
            }
            if !callbacks.contains(&handler.callback) {
                return Err(GenerationError::new(
                    handler.offset,
                    format!(
                        "event `{}` references undeclared callback `{}`",
                        handler.event, handler.callback
                    ),
                ));
            }
            let event = at(handler.offset, event_name(&node.kind, &handler.event))?;
            let callback = at(handler.offset, rust_ident(&handler.callback))?;
            setup.extend(quote!(events.push(dom.listen(&#variable, #event, #callback.clone())?);));
        }
        let children = emit_nodes(
            &node.children,
            quote!(#variable),
            properties,
            callbacks,
            sequence,
        )?;
        let tag = spec.tag;
        let class = spec.class;
        output.extend(quote! {
            let #variable = dom.element(#tag, #class)?;
            #setup
            #children
            dom.append(&#parent, &#variable)?;
        });
    }
    Ok(output)
}

fn emit_property(
    variable: &syn::Ident,
    kind: &str,
    name: &str,
    value: &Value,
    properties: &HashMap<String, PropertyKind>,
) -> Result<TokenStream, String> {
    match name {
        "text" => match value {
            Value::String(text) if matches!(kind, "LineEdit" | "TextInput") => {
                Ok(quote!(dom.attribute(&#variable, "value", #text)?;))
            }
            Value::String(text) => Ok(quote!(dom.text(&#variable, #text);)),
            Value::Identifier(binding) => {
                require_binding(properties, binding, PropertyKind::String, name)?;
                let binding = rust_ident(binding)?;
                if matches!(kind, "LineEdit" | "TextInput") {
                    Ok(quote!(events.push(dom.bind_input(&#variable, &#binding)?);))
                } else {
                    Ok(quote!(subscriptions.push(dom.bind_text(&#variable, &#binding));))
                }
            }
            _ => Err(format!(
                "`text` on `{kind}` requires a string or string property"
            )),
        },
        "enabled"
            if !matches!(
                kind,
                "Button" | "TouchArea" | "LineEdit" | "TextInput" | "CheckBox" | "Slider"
            ) =>
        {
            // `disabled` has no effect on the spans and divs used elsewhere.
            Err(format!("`enabled` is not supported on `{kind}`"))
        }
        "enabled" | "visible" => {
            let attribute = if name == "enabled" {
                "disabled"
            } else {
                "hidden"
            };
            match value {
                Value::Bool(value) => {
                    let present = !value;
                    Ok(quote!(dom.boolean_attribute(&#variable, #attribute, #present);))
                }
                Value::Identifier(binding) => {
                    require_binding(properties, binding, PropertyKind::Bool, name)?;
                    let binding = rust_ident(binding)?;
                    if name == "enabled" {
                        Ok(quote!(subscriptions.push(dom.bind_enabled(&#variable, &#binding));))
                    } else {
                        Ok(quote!(subscriptions.push(dom.bind_visible(&#variable, &#binding));))
                    }
                }
                Value::NotIdentifier(binding) => {
                    require_binding(properties, binding, PropertyKind::Bool, name)?;
                    let binding = rust_ident(binding)?;
                    if name == "enabled" {
                        Ok(
                            quote!(subscriptions.push(dom.bind_enabled_inverted(&#variable, &#binding));),
                        )
                    } else {
                        Ok(
                            quote!(subscriptions.push(dom.bind_visible_inverted(&#variable, &#binding));),
                        )
                    }
                }
                _ => Err(format!("`{name}` requires a bool or bool property")),
            }
        }
        "checked" => match value {
            Value::Bool(v) => Ok(quote!(dom.boolean_attribute(&#variable, "checked", #v);)),
            Value::Identifier(binding) => {
                require_binding(properties, binding, PropertyKind::Bool, name)?;
                let binding = rust_ident(binding)?;
                Ok(quote!(events.push(dom.bind_checked(&#variable, &#binding)?);))
            }
            _ => Err("`checked` requires a bool or bool property".into()),
        },
        "placeholder-text" => string_attribute(variable, "placeholder", value),
        "accessible-label" => string_attribute(variable, "aria-label", value),
        "accessible-role" => accessible_role(variable, value),
        "accessible-live-region" => accessible_live_region(variable, value),
        "source" => string_attribute(variable, "src", value),
        "value" if kind == "Slider" => slider_value(variable, value, properties),
        "value" | "minimum" | "maximum" | "step" => scalar_attribute(
            variable,
            match name {
                "minimum" => "min",
                "maximum" => "max",
                other => other,
            },
            value,
        ),
        "width" | "height" | "min-width" | "min-height" | "max-width" | "max-height"
        | "padding" | "spacing" | "background" | "border-radius" => {
            let css_name = if name == "spacing" { "gap" } else { name };
            let value = static_value(value)?;
            validate_css_value(css_name, &value)?;
            Ok(quote!(dom.style(&#variable, #css_name, #value)?;))
        }
        _ => Err(format!("unsupported property `{name}`")),
    }
}

fn slider_value(
    variable: &syn::Ident,
    value: &Value,
    properties: &HashMap<String, PropertyKind>,
) -> Result<TokenStream, String> {
    match value {
        Value::Identifier(binding) => match properties.get(binding) {
            Some(PropertyKind::Float) => {
                let binding = rust_ident(binding)?;
                Ok(quote!(events.push(dom.bind_slider_f64(&#variable, &#binding)?);))
            }
            Some(PropertyKind::Int) => {
                let binding = rust_ident(binding)?;
                Ok(quote!(events.push(dom.bind_slider_i32(&#variable, &#binding)?);))
            }
            Some(_) => Err(format!(
                "property `{binding}` has the wrong type for Slider.value"
            )),
            None => Err(format!("unknown property binding `{binding}`")),
        },
        _ => scalar_attribute(variable, "value", value),
    }
}

fn string_attribute(
    variable: &syn::Ident,
    attribute: &str,
    value: &Value,
) -> Result<TokenStream, String> {
    match value {
        Value::String(v) => Ok(quote!(dom.attribute(&#variable, #attribute, #v)?;)),
        _ => Err(format!("`{attribute}` requires a string literal")),
    }
}

fn accessible_role(variable: &syn::Ident, value: &Value) -> Result<TokenStream, String> {
    let Value::Identifier(value) = value else {
        return Err(
            "`accessible-role` requires a Slint enum value such as `text` or `button` (without quotes)"
                .into(),
        );
    };

    // Slint and ARIA use slightly different names for several equivalent roles.
    // `text` has no corresponding ARIA role; a plain HTML text element already
    // provides the intended semantics.
    let role = match value.as_str() {
        "text" => return Ok(TokenStream::new()),
        "none" | "button" | "checkbox" | "combobox" | "list" | "slider" | "tab" | "table"
        | "tree" | "switch" | "banner" | "complementary" | "form" | "main" | "navigation"
        | "region" | "search" => value.as_str(),
        "groupbox" | "radio-group" => "group",
        "image" => "img",
        "spinbox" => "spinbutton",
        "tab-list" => "tablist",
        "tab-panel" => "tabpanel",
        "progress-indicator" => "progressbar",
        "text-input" => "textbox",
        "list-item" => "listitem",
        "radio-button" => "radio",
        "window-title-bar" => "toolbar",
        "content-info" => "contentinfo",
        other => return Err(format!("unsupported Slint accessible role `{other}`")),
    };
    Ok(quote!(dom.attribute(&#variable, "role", #role)?;))
}

fn accessible_live_region(variable: &syn::Ident, value: &Value) -> Result<TokenStream, String> {
    let Value::Identifier(value) = value else {
        return Err(
            "`accessible-live-region` requires `off`, `polite`, or `assertive` (without quotes)"
                .into(),
        );
    };
    if !matches!(value.as_str(), "off" | "polite" | "assertive") {
        return Err(format!(
            "unsupported accessible live-region value `{value}`; use `off`, `polite`, or `assertive`"
        ));
    }
    Ok(quote!(dom.attribute(&#variable, "aria-live", #value)?;))
}

fn scalar_attribute(
    variable: &syn::Ident,
    attribute: &str,
    value: &Value,
) -> Result<TokenStream, String> {
    let value = match value {
        Value::Number(value) if value.parse::<f64>().is_ok() => value.clone(),
        _ => return Err(format!("`{attribute}` requires a unitless number literal")),
    };
    Ok(quote!(dom.attribute(&#variable, #attribute, #value)?;))
}

fn validate_css_value(name: &str, value: &str) -> Result<(), String> {
    if name == "background" {
        if let Some(digits) = value.strip_prefix('#') {
            if !matches!(digits.len(), 3 | 4 | 6 | 8)
                || !digits.chars().all(|c| c.is_ascii_hexdigit())
            {
                return Err(format!("invalid CSS color `{value}`"));
            }
        }
        if value.starts_with('#')
            || matches!(
                value,
                "transparent" | "black" | "white" | "red" | "green" | "blue"
            )
        {
            return Ok(());
        }
        return Err(format!("unsupported background `{value}`; use a hex color"));
    }
    if value == "0" {
        return Ok(());
    }
    for unit in ["px", "rem", "em", "%", "vh", "vw"] {
        if value.strip_suffix(unit).is_some_and(|number| {
            number
                .parse::<f64>()
                .is_ok_and(|value| value.is_finite() && value >= 0.0)
        }) {
            return Ok(());
        }
    }
    Err(format!("invalid CSS size `{value}` for `{name}`"))
}
fn static_value(value: &Value) -> Result<String, String> {
    match value {
        Value::String(v) | Value::Number(v) => Ok(v.clone()),
        Value::Bool(v) => Ok(v.to_string()),
        Value::Identifier(v) | Value::NotIdentifier(v) => Err(format!(
            "dynamic binding `{v}` is not supported for this property"
        )),
    }
}

fn require_binding(
    properties: &HashMap<String, PropertyKind>,
    binding: &str,
    expected: PropertyKind,
    target: &str,
) -> Result<(), String> {
    match properties.get(binding) {
        Some(actual) if *actual == expected => Ok(()),
        Some(_) => Err(format!(
            "property `{binding}` has the wrong type for `{target}`"
        )),
        None => Err(format!("unknown property binding `{binding}`")),
    }
}

fn validate(component: &Component) -> GenerationResult<()> {
    at(component.offset, rust_ident(&component.name))?;
    let mut fields: HashSet<String> = [
        "root",
        "_events",
        "_subscriptions",
        "dom",
        "events",
        "subscriptions",
        "parent",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let mut methods: HashSet<String> = ["mount", "mount_to_body", "root", "unmount"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    for property in &component.properties {
        at(property.offset, rust_ident(&property.name))?;
        let name = normalized(&property.name);
        if !fields.insert(name.clone()) {
            return Err(GenerationError::new(
                property.offset,
                format!("duplicate component member `{}`", property.name),
            ));
        }
        for method in [&name, &format!("set_{name}"), &format!("{name}_property")] {
            if !methods.insert(method.clone()) {
                return Err(GenerationError::new(
                    property.offset,
                    format!("generated method name `{method}` is used more than once"),
                ));
            }
        }
    }
    for callback in &component.callbacks {
        at(callback.offset, rust_ident(&callback.name))?;
        let name = normalized(&callback.name);
        if !fields.insert(name.clone()) {
            return Err(GenerationError::new(
                callback.offset,
                format!("duplicate component member `{}`", callback.name),
            ));
        }
        let method = format!("on_{name}");
        if !methods.insert(method.clone()) {
            return Err(GenerationError::new(
                callback.offset,
                format!("generated method name `{method}` is used more than once"),
            ));
        }
    }
    let mut ids = Vec::new();
    collect_ids(&component.children, &mut ids);
    for (id, offset) in ids {
        at(offset, rust_ident(id))?;
        let name = normalized(id);
        if !fields.insert(name.clone()) || !methods.insert(name) {
            return Err(GenerationError::new(
                offset,
                format!("duplicate element id or component member `{id}`"),
            ));
        }
    }
    Ok(())
}

fn collect_ids<'a>(elements: &'a [Element], output: &mut Vec<(&'a str, usize)>) {
    for element in elements {
        if let Some(id) = &element.id {
            output.push((id, element.offset));
        }
        collect_ids(&element.children, output);
    }
}

fn rust_ident(name: &str) -> Result<syn::Ident, String> {
    if name.starts_with("__node_") {
        return Err(format!("`{name}` uses a reserved generated prefix"));
    }
    syn::parse_str::<syn::Ident>(&normalized(name))
        .map_err(|_| format!("`{name}` cannot be exposed as a Rust identifier"))
}
fn normalized(name: &str) -> String {
    name.replace('-', "_")
}

fn rust_type(kind: PropertyKind) -> TokenStream {
    match kind {
        PropertyKind::String => quote!(::std::string::String),
        PropertyKind::Bool => quote!(bool),
        PropertyKind::Int => quote!(i32),
        PropertyKind::Float => quote!(f64),
    }
}
fn literal(value: &Value, kind: PropertyKind) -> Result<TokenStream, String> {
    match (value, kind) {
        (Value::String(v), PropertyKind::String) => Ok(quote!(::std::string::String::from(#v))),
        (Value::Bool(v), PropertyKind::Bool) => Ok(quote!(#v)),
        (Value::Number(v), PropertyKind::Int) => v
            .parse::<i32>()
            .map(|v| quote!(#v))
            .map_err(|_| format!("`{v}` is not a valid int")),
        // Out-of-range literals such as `1e999` parse to infinity, which
        // cannot be emitted as a Rust literal.
        (Value::Number(v), PropertyKind::Float) => v
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|v| quote!(#v))
            .ok_or_else(|| format!("`{v}` is not a valid float")),
        _ => Err("property initial value has the wrong type".into()),
    }
}

struct Widget {
    tag: &'static str,
    class: &'static str,
    input_type: Option<&'static str>,
    properties: &'static [&'static str],
}
const COMMON_PROPERTIES: &[&str] = &[
    "enabled",
    "visible",
    "width",
    "height",
    "min-width",
    "min-height",
    "max-width",
    "max-height",
    "background",
    "border-radius",
    "accessible-label",
    "accessible-role",
    "accessible-live-region",
];

fn widget(kind: &str) -> Result<Widget, String> {
    let result = match kind {
        "VerticalLayout" => Widget {
            tag: "div",
            class: "sd-column",
            input_type: None,
            properties: &["padding", "spacing"],
        },
        "HorizontalLayout" => Widget {
            tag: "div",
            class: "sd-row",
            input_type: None,
            properties: &["padding", "spacing"],
        },
        "GridLayout" => Widget {
            tag: "div",
            class: "sd-grid",
            input_type: None,
            properties: &["padding", "spacing"],
        },
        "Text" => Widget {
            tag: "span",
            class: "sd-text",
            input_type: None,
            properties: &["text"],
        },
        "Button" => Widget {
            tag: "button",
            class: "sd-button",
            input_type: Some("button"),
            properties: &["text"],
        },
        "LineEdit" | "TextInput" => Widget {
            tag: "input",
            class: "sd-input",
            input_type: Some("text"),
            properties: &["text", "placeholder-text"],
        },
        "CheckBox" => Widget {
            tag: "input",
            class: "sd-checkbox",
            input_type: Some("checkbox"),
            properties: &["checked"],
        },
        "Slider" => Widget {
            tag: "input",
            class: "sd-slider",
            input_type: Some("range"),
            properties: &["value", "minimum", "maximum", "step"],
        },
        "Image" => Widget {
            tag: "img",
            class: "sd-image",
            input_type: None,
            properties: &["source"],
        },
        "Rectangle" => Widget {
            tag: "div",
            class: "sd-rectangle",
            input_type: None,
            properties: &[],
        },
        "TouchArea" => Widget {
            tag: "button",
            class: "sd-touch",
            input_type: Some("button"),
            properties: &[],
        },
        other => return Err(format!("unsupported Slint element `{other}`")),
    };
    Ok(result)
}

fn event_name(kind: &str, event: &str) -> Result<&'static str, String> {
    match (kind, event) {
        ("Button" | "TouchArea", "clicked") => Ok("click"),
        ("LineEdit" | "TextInput", "accepted") => Ok("accepted"),
        ("LineEdit" | "TextInput", "edited") => Ok("input"),
        ("CheckBox", "toggled") => Ok("change"),
        _ => Err(format!("event `{event}` is not supported on `{kind}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;
    #[test]
    fn rejects_internal_names_and_invalid_css() {
        for name in ["dom", "events", "subscriptions", "parent", "__node_0"] {
            let source = format!("export component App {{ property <bool> {name}: true; }}");
            let input = parser::parse(&source).unwrap();
            assert!(
                component(
                    &input,
                    &LitStr::new("ui.slint", proc_macro2::Span::call_site())
                )
                .is_err(),
                "{name}"
            );
        }
        for value in ["12", "NaNpx", "-2px", "12oops"] {
            assert!(validate_css_value("width", value).is_err(), "{value}");
        }
    }
    #[test]
    fn output_contains_state_callback_and_real_dom_calls() {
        let input = parser::parse(r#"export component App inherits Window { property <bool> enabled: true; callback go(); Button { enabled: enabled; clicked => { root.go(); } } }"#).unwrap();
        let output = component(
            &input,
            &LitStr::new("ui.slint", proc_macro2::Span::call_site()),
        )
        .unwrap()
        .to_string();
        assert!(
            output.contains("set_enabled")
                && output.contains("on_go")
                && output.contains("bind_enabled")
                && output.contains("dom . listen")
        );
    }

    #[test]
    fn rejects_generated_api_name_collisions() {
        let input =
            parser::parse("export component App { property <bool> root: true; Rectangle {} }")
                .unwrap();
        let error = component(
            &input,
            &LitStr::new("ui.slint", proc_macro2::Span::call_site()),
        )
        .unwrap_err();
        assert!(error.contains("duplicate component member"));
    }

    #[test]
    fn validates_css_sizes_and_colors() {
        assert!(validate_css_value("width", "12px").is_ok());
        assert!(validate_css_value("width", "12oops").is_err());
        assert!(validate_css_value("background", "#12xx00").is_err());
    }

    #[test]
    fn accepts_slint_accessibility_enum_syntax() {
        let input = parser::parse(
            "export component App { Text { text: \"Ready\"; accessible-role: text; accessible-live-region: polite; } Rectangle { accessible-role: image; accessible-label: \"Chart\"; } }",
        )
        .unwrap();
        let output = component(
            &input,
            &LitStr::new("ui.slint", proc_macro2::Span::call_site()),
        )
        .unwrap()
        .to_string();
        assert!(output.contains("aria-live"));
        assert!(output.contains("polite"));
        assert!(output.contains("role"));
        assert!(output.contains("img"));
    }

    #[test]
    fn rejects_quoted_or_unknown_accessibility_enums() {
        for source in [
            "export component App { Text { accessible-role: \"text\"; } }",
            "export component App { Text { accessible-role: status; } }",
            "export component App { Text { accessible-live-region: loud; } }",
        ] {
            let input = parser::parse(source).unwrap();
            assert!(
                component(
                    &input,
                    &LitStr::new("ui.slint", proc_macro2::Span::call_site())
                )
                .is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn slider_value_is_bound_after_its_bounds() {
        let input = parser::parse(
            "export component App { property <float> level: 150; Slider { value: level; maximum: 200; } }",
        )
        .unwrap();
        let output = component(
            &input,
            &LitStr::new("ui.slint", proc_macro2::Span::call_site()),
        )
        .unwrap()
        .to_string();
        let maximum = output.find("\"max\"").unwrap();
        let value = output.find("bind_slider_f64").unwrap();
        assert!(maximum < value, "{output}");
    }

    #[test]
    fn rejects_enabled_on_non_controls() {
        let input = parser::parse("export component App { Text { text: \"x\"; enabled: false; } }")
            .unwrap();
        let error = component(
            &input,
            &LitStr::new("ui.slint", proc_macro2::Span::call_site()),
        )
        .unwrap_err();
        assert!(
            error.contains("`enabled` is not supported on `Text`"),
            "{error}"
        );
    }

    #[test]
    fn rejects_out_of_range_float_literals() {
        let input = parser::parse("export component App { property <float> x: 1e999; }").unwrap();
        let error = component(
            &input,
            &LitStr::new("ui.slint", proc_macro2::Span::call_site()),
        )
        .unwrap_err();
        assert!(error.contains("`1e999` is not a valid float"), "{error}");
    }

    #[test]
    fn rejects_children_of_void_elements() {
        let input =
            parser::parse("export component App { LineEdit { Text { text: \"invalid\"; } } }")
                .unwrap();
        let error = component(
            &input,
            &LitStr::new("ui.slint", proc_macro2::Span::call_site()),
        )
        .unwrap_err();
        assert!(error.contains("cannot contain children"));
    }

    #[test]
    fn expected_failed() {
        let input_result = parser::parse("property <string> status: \"Ready\";   export component App { Text { text: status; } }");
        println!("------------------ Input: {input_result:?}");
        let input = input_result.err().unwrap();
        println!("------------------ Error: {input:?}");
        let m = format!("{input:?}");

        assert!(
            m.contains("ParseError { message: \"expected `component` declaration\", offset: 9 ")
        );
    }

    #[test]
    fn is_supported() {
        let input_result = parser::parse(
            "export component App { property <string> status: \"Ready\"; Text { text: status; } }",
        );
        println!("------------------ Input: {input_result:?}");
        let input = input_result.unwrap();
        let r = component(
            &input,
            &LitStr::new("ui.slint", proc_macro2::Span::call_site()),
        );

        //        println!("------------------ Result: {r:?}");

        let rt = r.unwrap().to_string();
        println!("------------------ Result String: {rt}");

        assert!(rt.contains("let status = :: slint_dom :: Property :: new (:: std :: string :: String :: from (\"Ready\")) ;"));
    }

    #[test]
    fn rejects_unsupported_elements() {
        let input = parser::parse(
            "export component App { property <string> status: \"Ready\"; Chart { } }",
        )
        .unwrap();
        let error = component(
            &input,
            &LitStr::new("ui.slint", proc_macro2::Span::call_site()),
        )
        .unwrap_err();
        assert!(error.contains("unsupported Slint element `Chart`"));
    }
}
