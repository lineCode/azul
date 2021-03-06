use std::{
    collections::BTreeMap,
    rc::Rc,
    cell::RefCell,
    sync::{Arc, Mutex},
};
use {
    dom::{NodeData, Dom},
    ui_description::{StyledNode, CssConstraintList, UiDescription},
    css::{Css, CssRule},
    id_tree::{NodeId, Arena},
    css_parser::{ParsedCssProperty, CssParsingError},
};
#[cfg(not(test))]
use window::WindowInfo;

/// The core trait that has to be implemented for the app model to provide a
/// Model -> View serialization.
pub trait Layout {
    /// Updates the DOM, must be provided by the final application.
    ///
    /// On each frame, a completely new DOM tree is generated. The final
    /// application can cache the DOM tree, but this isn't in the scope of `azul`.
    ///
    /// The `style_dom` looks through the given DOM rules, applies the style and
    /// recalculates the layout. This is done on each frame (except there are shortcuts
    /// when the DOM doesn't have to be recalculated).
    #[cfg(not(test))]
    fn layout(&self, window_id: WindowInfo) -> Dom<Self> where Self: Sized;
    #[cfg(test)]
    fn layout(&self) -> Dom<Self> where Self: Sized;
    /// Applies the CSS styles to the nodes calculated from the `layout_screen`
    /// function and calculates the final display list that is submitted to the
    /// renderer.
    fn style_dom(dom: &Dom<Self>, css: &Css) -> UiDescription<Self> where Self: Sized {
        match_dom_css_selectors(dom.root, &dom.arena, &ParsedCss::from_css(css), css, 0)
    }
}

pub(crate) struct ParsedCss<'a> {
    pub(crate) pure_global_rules: Vec<&'a CssRule>,
    pub(crate) pure_div_rules: Vec<&'a CssRule>,
    pub(crate) pure_class_rules: Vec<&'a CssRule>,
    pub(crate) pure_id_rules: Vec<&'a CssRule>,
}

/// Convenience trait for the `css.set_dynamic_property()` function.
///
/// This trait exists because `TryFrom` / `TryInto` are not yet stabilized.
/// This is the same as `Into<ParsedCssProperty>`, but with an additional error
/// case (since the parsing of the CSS value could potentially fail)
///
/// Using this trait you can write: `css.set_dynamic_property("var", ("width", "500px"))`
/// because `IntoParsedCssProperty` is implemented for `(&str, &str)`.
///
/// Note that the properties have to be re-parsed on every frame (which incurs a
/// small per-frame performance hit), however `("width", "500px")` is easier to
/// read than `ParsedCssProperty::Width(PixelValue::Pixels(500))`
pub trait IntoParsedCssProperty<'a> {
    fn into_parsed_css_property(self) -> Result<ParsedCssProperty, CssParsingError<'a>>;
}

/// Convenience trait that allows the `app_state.modify()` - only implemented for
/// `Arc<Mutex<T>` - shortly locks the app state mutex, modifies it and unlocks
/// it again.
///
/// Note: Usually when doing asynchronous programming you don't want to block the main
/// UI. While Rust executes the `app_state.modify()` closure, your `AppState` gets
/// locked, meaning that no layout can happen and no other thread or callback can write
/// to the apps data. In order to make your app performant, don't do heavy computations
/// inside the closure, only use it to write or copy data in and out of the application
/// state.
pub trait Modify<T> {
    /// Modifies the app state and then returns if the modification was successful
    /// Takes a FnMut that modifies the state
    fn modify<F>(&self, closure: F) -> bool where F: FnOnce(&mut T);
}

impl<T> Modify<T> for Arc<Mutex<T>> {
    fn modify<F>(&self, closure: F) -> bool where F: FnOnce(&mut T) {
        match self.lock().as_mut() {
            Ok(lock) => { closure(&mut *lock); true },
            Err(_) => false,
        }
    }
}

impl<'a> IntoParsedCssProperty<'a> for ParsedCssProperty {
    fn into_parsed_css_property(self) -> Result<ParsedCssProperty, CssParsingError<'a>> {
        Ok(self.clone())
    }
}

impl<'a> IntoParsedCssProperty<'a> for (&'a str, &'a str) {
    fn into_parsed_css_property(self) -> Result<ParsedCssProperty, CssParsingError<'a>> {
        ParsedCssProperty::from_kv(self.0, self.1)
    }
}

impl<'a> ParsedCss<'a> {
    pub(crate) fn from_css(css: &'a Css) -> Self {

        // Parse the CSS nodes cascading by their importance
        // 1. global rules
        // 2. div-type ("html { }") specific rules
        // 3. class-based rules
        // 4. ID-based rules

        /*
            CssRule { html_type: "div", id: Some("main"), classes: [], declaration: ("direction", "row") }
            CssRule { html_type: "div", id: Some("main"), classes: [], declaration: ("justify-content", "center") }
            CssRule { html_type: "div", id: Some("main"), classes: [], declaration: ("align-items", "center") }
            CssRule { html_type: "div", id: Some("main"), classes: [], declaration: ("align-content", "center") }
        */

        // note: the following passes can be done in parallel ...

        // Global rules
        // * {
        //    background-color: blue;
        // }
        let pure_global_rules: Vec<&CssRule> = css.rules.iter().filter(|rule|
            rule.html_type == "*" && rule.id.is_none() && rule.classes.is_empty()
        ).collect();

        // Pure-div-type specific rules
        // button {
        //    justify-content: center;
        // }
        let pure_div_rules: Vec<&CssRule> = css.rules.iter().filter(|rule|
            rule.html_type != "*" && rule.id.is_none() && rule.classes.is_empty()
        ).collect();

        // Pure-class rules
        // NOTE: These classes are sorted alphabetically and are not duplicated
        //
        // .something .otherclass {
        //    text-color: red;
        // }
        let pure_class_rules: Vec<&CssRule> = css.rules.iter().filter(|rule|
            rule.id.is_none() && !rule.classes.is_empty()
        ).collect();

        // Pure-id rules
        // #something {
        //    background-color: red;
        // }
        let pure_id_rules: Vec<&CssRule> = css.rules.iter().filter(|rule|
            rule.id.is_some() && rule.classes.is_empty()
        ).collect();

        Self {
            pure_global_rules: pure_global_rules,
            pure_div_rules: pure_div_rules,
            pure_class_rules: pure_class_rules,
            pure_id_rules: pure_id_rules,
        }
    }
}

fn match_dom_css_selectors<'a, T: Layout>(
    root: NodeId,
    arena: &Rc<RefCell<Arena<NodeData<T>>>>,
    parsed_css: &ParsedCss<'a>,
    css: &Css,
    parent_z_level: u32)
-> UiDescription<T>
{
    let mut root_constraints = CssConstraintList::default();
    for global_rule in &parsed_css.pure_global_rules {
        root_constraints.push_rule(global_rule);
    }

    let arena_borrow = &*(*arena).borrow();
    let mut styled_nodes = BTreeMap::<NodeId, StyledNode>::new();
    let sibling_iterator = root.following_siblings(arena_borrow);
    // skip the root node itself, see documentation for `following_siblings` in id_tree.rs
    // sibling_iterator.next().unwrap();

    for sibling in sibling_iterator {
        styled_nodes.append(&mut match_dom_css_selectors_inner(sibling, arena_borrow, parsed_css, css, &root_constraints, parent_z_level));
    }

    UiDescription {
        // note: this clone is neccessary, otherwise,
        // we wouldn't be able to update the UiState
        ui_descr_arena: (*arena).clone(),
        ui_descr_root: Some(root),
        styled_nodes: styled_nodes,
        default_style_of_node: StyledNode::default(),
        dynamic_css_overrides: css.dynamic_css_overrides.clone(),
    }
}

fn match_dom_css_selectors_inner<'a, T: Layout>(
    root: NodeId,
    arena: &Arena<NodeData<T>>,
    parsed_css: &ParsedCss<'a>,
    css: &Css,
    parent_constraints: &CssConstraintList,
    parent_z_level: u32)
-> BTreeMap<NodeId, StyledNode>
{
    let mut styled_nodes = BTreeMap::<NodeId, StyledNode>::new();

    let mut current_constraints = CssConstraintList {
        list: parent_constraints.list.iter().filter(|prop| prop.is_inheritable()).cloned().collect(),
    };

    cascade_constraints(&arena[root].data, &mut current_constraints, parsed_css, css);

    let current_node = StyledNode {
        z_level: parent_z_level,
        css_constraints: current_constraints,
    };

    // DFS tree
    for child in root.children(arena) {
        styled_nodes.append(&mut match_dom_css_selectors_inner(child, arena, parsed_css, css, &current_node.css_constraints, parent_z_level + 1));
    }

    styled_nodes.insert(root, current_node);
    styled_nodes
}

/// Cascade the rules, put them into the list
#[allow(unused_variables)]
fn cascade_constraints<'a, T: Layout>(
    node: &NodeData<T>,
    list: &mut CssConstraintList,
    parsed_css: &ParsedCss<'a>,
    css: &Css)
{
    for div_rule in &parsed_css.pure_div_rules {
        if *node.node_type.get_css_id() == div_rule.html_type {
            list.push_rule(div_rule);
        }
    }

    let mut node_classes: Vec<&String> = node.classes.iter().map(|x| x).collect();
    node_classes.sort();
    node_classes.dedup_by(|a, b| *a == *b);

    // for all classes that this node has
    for class_rule in &parsed_css.pure_class_rules {
        // NOTE: class_rule is sorted and de-duplicated
        // If the selector matches, the node classes must be identical
        let mut should_insert_rule = true;
        if class_rule.classes.len() != node_classes.len() {
            should_insert_rule = false;
        } else {
            for i in 0..class_rule.classes.len() {
                // we verified that the length of the two classes is the same
                if *node_classes[i] != class_rule.classes[i] {
                    should_insert_rule = false;
                    break;
                }
            }
        }

        if should_insert_rule {
            list.push_rule(class_rule);
        }
    }

    // first attribute for "id = something"
    let node_id = &node.id;

    if let Some(ref node_id) = *node_id {
        // if the node has an ID
        for id_rule in &parsed_css.pure_id_rules {
            if *id_rule.id.as_ref().unwrap() == *node_id {
                list.push_rule(id_rule);
            }
        }
    }

    // TODO: all the mixed rules
}

// Empty test, for some reason codecov doesn't detect any files (and therefore
// doesn't report codecov % correctly) except if they have at least one test in
// the file. This is an empty test, which should be updated later on
#[test]
fn __codecov_test_traits_file() {

}