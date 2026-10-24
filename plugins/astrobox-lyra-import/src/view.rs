//! A plain tree of the host's UI elements. Pages are built as `View`s and turned into
//! `ui::Element`s only when rendered, so tests can build them natively and dump them in the shape
//! the AstroBox front end receives (`PluginUiElement`) for previews.

use crate::astrobox::psys_host_v4::ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Div,
    P,
    Span,
    Button,
    Input,
    Select,
    Option,
    Image,
    Svg,
    Progress,
    Card,
    TabsRoot,
    TabsList,
    TabsTrigger,
    TabsContent,
    MenuRoot,
    MenuTrigger,
    MenuContent,
    MenuItem,
    MenuSeparator,
    Separator,
    Badge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum On {
    Click,
    Change,
    Input,
}

/// One style setter of `ui::Element`, with its argument.
#[derive(Clone, Debug, PartialEq)]
enum Style {
    Flex,
    Row,
    Column,
    Gap(u32),
    Padding(u32),
    PaddingX(u32),
    PaddingY(u32),
    MarginTop(u32),
    AlignCenter,
    AlignStart,
    JustifyCenter,
    Bg(String),
    Color(String),
    FontSize(u32),
    Width(u32),
    WidthFull,
    Height(u32),
    MaxWidth(u32),
    MinWidth(u32),
    Radius(u32),
    Grow(f32),
    Shrink(f32),
    Disabled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    kind: Kind,
    content: Option<String>,
    props: Vec<(String, String)>,
    styles: Vec<Style>,
    events: Vec<(On, String)>,
    children: Vec<View>,
}

impl View {
    pub fn new(kind: Kind, content: Option<&str>) -> Self {
        Self {
            kind,
            content: content.map(str::to_string),
            props: Vec::new(),
            styles: Vec::new(),
            events: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn prop(mut self, name: &str, value: &str) -> Self {
        self.props.push((name.to_string(), value.to_string()));
        self
    }

    pub fn on(mut self, event: On, id: &str) -> Self {
        self.events.push((event, id.to_string()));
        self
    }

    pub fn child(mut self, child: View) -> Self {
        self.children.push(child);
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = View>) -> Self {
        self.children.extend(children);
        self
    }

    /// Adds the child when there is one.
    pub fn maybe(self, child: Option<View>) -> Self {
        match child {
            Some(child) => self.child(child),
            None => self,
        }
    }

    fn style(mut self, style: Style) -> Self {
        self.styles.push(style);
        self
    }

    pub fn row(self) -> Self {
        self.style(Style::Flex).style(Style::Row)
    }

    pub fn column(self) -> Self {
        self.style(Style::Flex).style(Style::Column)
    }

    pub fn gap(self, gap: u32) -> Self {
        self.style(Style::Gap(gap))
    }

    pub fn padding(self, padding: u32) -> Self {
        self.style(Style::Padding(padding))
    }

    pub fn padding_x(self, padding: u32) -> Self {
        self.style(Style::PaddingX(padding))
    }

    pub fn padding_y(self, padding: u32) -> Self {
        self.style(Style::PaddingY(padding))
    }

    pub fn margin_top(self, margin: u32) -> Self {
        self.style(Style::MarginTop(margin))
    }

    pub fn align_center(self) -> Self {
        self.style(Style::AlignCenter)
    }

    pub fn align_start(self) -> Self {
        self.style(Style::AlignStart)
    }

    pub fn justify_center(self) -> Self {
        self.style(Style::JustifyCenter)
    }

    pub fn bg(self, color: &str) -> Self {
        self.style(Style::Bg(color.to_string()))
    }

    pub fn color(self, color: &str) -> Self {
        self.style(Style::Color(color.to_string()))
    }

    pub fn font_size(self, size: u32) -> Self {
        self.style(Style::FontSize(size))
    }

    pub fn width(self, width: u32) -> Self {
        self.style(Style::Width(width))
    }

    pub fn width_full(self) -> Self {
        self.style(Style::WidthFull)
    }

    pub fn height(self, height: u32) -> Self {
        self.style(Style::Height(height))
    }

    pub fn square(self, side: u32) -> Self {
        self.width(side).height(side).style(Style::Shrink(0.0))
    }

    pub fn max_width(self, width: u32) -> Self {
        self.style(Style::MaxWidth(width))
    }

    pub fn radius(self, radius: u32) -> Self {
        self.style(Style::Radius(radius))
    }

    /// Takes the free space of a row; `min-width: 0` lets its text truncate.
    pub fn grow(self) -> Self {
        self.style(Style::Grow(1.0)).style(Style::MinWidth(0))
    }

    pub fn disabled(self) -> Self {
        self.style(Style::Disabled)
    }

    pub fn disabled_if(self, disabled: bool) -> Self {
        if disabled { self.disabled() } else { self }
    }

    /// Adds Radix Themes utility classes (weights, alignment, truncation) the host page ships.
    pub fn class(mut self, class: &str) -> Self {
        match self.props.iter_mut().find(|(name, _)| name == "class") {
            Some((_, value)) => {
                value.push(' ');
                value.push_str(class);
            }
            None => self.props.push(("class".to_string(), class.to_string())),
        }
        self
    }

    pub fn into_element(self) -> ui::Element {
        let mut element = ui::Element::new(self.kind.into(), self.content.as_deref());
        for (name, value) in &self.props {
            element = element.prop(name, value);
        }
        for style in self.styles {
            element = match style {
                Style::Flex => element.flex(),
                Style::Row => element.flex_direction(ui::FlexDirection::Row),
                Style::Column => element.flex_direction(ui::FlexDirection::Column),
                Style::Gap(gap) => element.gap(gap),
                Style::Padding(padding) => element.padding(padding),
                Style::PaddingX(padding) => element.padding_left(padding).padding_right(padding),
                Style::PaddingY(padding) => element.padding_top(padding).padding_bottom(padding),
                Style::MarginTop(margin) => element.margin_top(margin),
                Style::AlignCenter => element.align_center(),
                Style::AlignStart => element.align_start(),
                Style::JustifyCenter => element.justify_center(),
                Style::Bg(color) => element.bg(&color),
                Style::Color(color) => element.text_color(&color),
                Style::FontSize(size) => element.size(size),
                Style::Width(width) => element.width(width),
                Style::WidthFull => element.width_full(),
                Style::Height(height) => element.height(height),
                Style::MaxWidth(width) => element.max_width(width),
                Style::MinWidth(width) => element.min_width(width),
                Style::Radius(radius) => element.radius(radius),
                Style::Grow(value) => element.flex_grow(value),
                Style::Shrink(value) => element.flex_shrink(value),
                Style::Disabled => element.disabled(),
            };
        }
        for (event, id) in &self.events {
            element = element.on((*event).into(), id);
        }
        for child in self.children {
            element = element.child(child.into_element());
        }
        element
    }
}

impl From<Kind> for ui::ElementType {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Div => ui::ElementType::Div,
            Kind::P => ui::ElementType::P,
            Kind::Span => ui::ElementType::Span,
            Kind::Button => ui::ElementType::Button,
            Kind::Input => ui::ElementType::Input,
            Kind::Select => ui::ElementType::Select,
            Kind::Option => ui::ElementType::Option,
            Kind::Image => ui::ElementType::Image,
            Kind::Svg => ui::ElementType::Svg,
            Kind::Progress => ui::ElementType::Progress,
            Kind::Card => ui::ElementType::Card,
            Kind::TabsRoot => ui::ElementType::TabsRoot,
            Kind::TabsList => ui::ElementType::TabsList,
            Kind::TabsTrigger => ui::ElementType::TabsTrigger,
            Kind::TabsContent => ui::ElementType::TabsContent,
            Kind::MenuRoot => ui::ElementType::DropdownMenuRoot,
            Kind::MenuTrigger => ui::ElementType::DropdownMenuTrigger,
            Kind::MenuContent => ui::ElementType::DropdownMenuContent,
            Kind::MenuItem => ui::ElementType::DropdownMenuItem,
            Kind::MenuSeparator => ui::ElementType::DropdownMenuSeparator,
            Kind::Separator => ui::ElementType::Separator,
            Kind::Badge => ui::ElementType::Badge,
        }
    }
}

impl From<On> for ui::Event {
    fn from(event: On) -> Self {
        match event {
            On::Click => ui::Event::Click,
            On::Change => ui::Event::Change,
            On::Input => ui::Event::Input,
        }
    }
}

#[cfg(test)]
impl View {
    /// The node as the host hands it to the front end (`PluginUiElement`), styles mapped the way
    /// the host's setters map them.
    pub fn to_json(&self, id: &mut u32) -> serde_json::Value {
        use serde_json::{Map, Value, json};
        *id += 1;
        let mut styles = Map::new();
        let mut set = |name: &str, value: String| {
            styles.insert(name.to_string(), Value::String(value));
        };
        let mut width = Value::Null;
        let mut height = Value::Null;
        for style in &self.styles {
            match style {
                Style::Flex => set("display", "flex".into()),
                Style::Row => set("flex-direction", "row".into()),
                Style::Column => set("flex-direction", "column".into()),
                Style::Gap(v) => set("gap", format!("{v}px")),
                Style::Padding(v) => set("padding", format!("{v}px")),
                Style::PaddingX(v) => {
                    set("padding-left", format!("{v}px"));
                    set("padding-right", format!("{v}px"));
                }
                Style::PaddingY(v) => {
                    set("padding-top", format!("{v}px"));
                    set("padding-bottom", format!("{v}px"));
                }
                Style::MarginTop(v) => set("margin-top", format!("{v}px")),
                Style::AlignCenter => set("align-items", "center".into()),
                Style::AlignStart => set("align-items", "flex-start".into()),
                Style::JustifyCenter => set("justify-content", "center".into()),
                Style::Bg(c) => set("background", c.clone()),
                Style::Color(c) => set("color", c.clone()),
                Style::FontSize(v) => set("font-size", format!("{v}px")),
                Style::Width(v) => {
                    width = json!(v);
                    set("width", format!("{v}px"));
                }
                Style::WidthFull => set("width", "100%".into()),
                Style::Height(v) => {
                    height = json!(v);
                    set("height", format!("{v}px"));
                }
                Style::MaxWidth(v) => set("max-width", format!("{v}px")),
                Style::MinWidth(v) => set("min-width", format!("{v}px")),
                Style::Radius(v) => set("border-radius", format!("{v}px")),
                Style::Grow(v) => set("flex-grow", v.to_string()),
                Style::Shrink(v) => set("flex-shrink", v.to_string()),
                Style::Disabled => set("disabled", "true".into()),
            }
        }
        let kind = match self.kind {
            Kind::Div => "DIV",
            Kind::P => "P",
            Kind::Span => "SPAN",
            Kind::Button => "BUTTON",
            Kind::Input => "INPUT",
            Kind::Select => "SELECT",
            Kind::Option => "OPTION",
            Kind::Image => "IMAGE",
            Kind::Svg => "SVG",
            Kind::Progress => "PROGRESS",
            Kind::Card => "CARD",
            Kind::TabsRoot => "TABS_ROOT",
            Kind::TabsList => "TABS_LIST",
            Kind::TabsTrigger => "TABS_TRIGGER",
            Kind::TabsContent => "TABS_CONTENT",
            Kind::MenuRoot => "DROPDOWN_MENU_ROOT",
            Kind::MenuTrigger => "DROPDOWN_MENU_TRIGGER",
            Kind::MenuContent => "DROPDOWN_MENU_CONTENT",
            Kind::MenuItem => "DROPDOWN_MENU_ITEM",
            Kind::MenuSeparator => "DROPDOWN_MENU_SEPARATOR",
            Kind::Separator => "SEPARATOR",
            Kind::Badge => "BADGE",
        };
        let own_id = *id;
        let children: Vec<Value> = self.children.iter().map(|child| child.to_json(id)).collect();
        json!({
            "id": format!("n{own_id}"),
            "type": kind,
            "content": self.content,
            "props": self.props.iter()
                .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                .collect::<Map<_, _>>(),
            "styles": styles,
            "width": width,
            "height": height,
            "event_listeners": self.events.iter().map(|(event, id)| json!({
                "id": id,
                "event": match event { On::Click => "CLICK", On::Change => "CHANGE", On::Input => "INPUT" },
            })).collect::<Vec<_>>(),
            "children": children,
        })
    }
}
