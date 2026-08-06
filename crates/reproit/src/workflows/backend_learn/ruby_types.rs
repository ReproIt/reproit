//! What a Rails action states it returns, read from the same parse the route
//! reader walks.
//!
//! Split from `ruby_ast` at the same boundary as `go_ast`/`go_types`: that file
//! resolves WHERE a request lands, this one resolves WHAT the action states it
//! writes back. Rails states a status far more readably than a body: `render
//! json: x, status: :created` and `head :no_content` name the code outright,
//! while the JSON shape of `x` is usually an ActiveModel or a serializer this
//! cannot follow. So a status is claimed wherever it is stated, and a shape only
//! where a literal hash or array is rendered; everything else is presence-only.

use super::field_facts::{drop_ambiguous, record};
use super::grammar::{self};
use super::response_facts::{literal_status, named_status, ResponseFact, WireShape};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

/// Response facts as the file walk collects them, keyed by action name, which
/// is exactly the handler the route reader records. Two controllers defining
/// the same action name differently abstain, the same rule as a model.
#[derive(Default)]
pub(super) struct Responses {
    facts: BTreeMap<String, ResponseFact>,
    ambiguous: BTreeSet<String>,
}

impl Responses {
    /// Read one parsed file: the response facts of every action in a controller.
    pub(super) fn take_file(&mut self, root: Node, text: &str) {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "class" && is_controller(node, text) {
                if let Some(body) = node.child_by_field_name("body") {
                    self.take_controller(body, text);
                }
            }
            let mut children = Vec::new();
            grammar::children(node, &mut children);
            stack.extend(children);
        }
    }

    /// Keep the response facts of routed actions, dropping ambiguous names.
    pub(super) fn finish(
        mut self,
        routes: &[(String, &'static str, Option<String>)],
    ) -> BTreeMap<String, ResponseFact> {
        drop_ambiguous(&mut self.facts, &self.ambiguous);
        let handlers: BTreeSet<&String> = routes
            .iter()
            .filter_map(|(_, _, handler)| handler.as_ref())
            .collect();
        self.facts.retain(|action, _| handlers.contains(action));
        self.facts
    }

    /// The actions a controller body defines, each with what its `render` and
    /// `head` calls state. A nested class inside the body is reached by the
    /// outer walk, so its actions are attributed to itself.
    fn take_controller(&mut self, body: Node, text: &str) {
        let mut members = Vec::new();
        grammar::children(body, &mut members);
        for member in members {
            if member.kind() != "method" {
                continue;
            }
            let Some(action) = grammar::field(member, text, "name") else {
                continue;
            };
            if let Some(fact) = action_response(member, text) {
                record(&mut self.facts, &mut self.ambiguous, action, fact);
            }
        }
    }
}

/// Whether a class is a controller: its name ends with `Controller`, so an
/// ordinary model's methods are not read as actions.
fn is_controller(node: Node, text: &str) -> bool {
    grammar::field(node, text, "name")
        .map(|name| {
            name.trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
                .ends_with("Controller")
        })
        .unwrap_or(false)
}

/// The responses one action states: its `render` and `head` calls. An action
/// that names no status and renders no literal states nothing.
fn action_response(method: Node, text: &str) -> Option<ResponseFact> {
    let mut fact = ResponseFact::default();
    grammar::walk(method, &mut |node| {
        if node.kind() != "call" {
            return;
        }
        match grammar::field(node, text, "method").as_deref() {
            Some("render") => render_call(node, text, &mut fact),
            Some("head") => head_call(node, text, &mut fact),
            _ => {}
        }
    });
    (!fact.statuses.is_empty()).then_some(fact)
}

/// `render json: x, status: :created`. The status defaults to 200 when the
/// call renders JSON without one; a `render` with neither a `json:` key nor a
/// `status:` key is an HTML render this states nothing about.
fn render_call(node: Node, text: &str, fact: &mut ResponseFact) {
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut args = Vec::new();
    grammar::children(arguments, &mut args);
    let mut json = None;
    let mut status = None;
    for pair in &args {
        if pair.kind() != "pair" {
            continue;
        }
        let value = pair.child_by_field_name("value");
        match grammar::field(*pair, text, "key").as_deref() {
            Some("json") => json = value,
            Some("status") => status = value.and_then(|node| symbol_or_int_status(node, text)),
            _ => {}
        }
    }
    match (json, status) {
        (Some(value), status) => fact.state(status.unwrap_or(200), json_shape(value)),
        (None, Some(status)) => fact.state(status, WireShape::Unknown),
        (None, None) => {}
    }
}

/// `head :no_content` / `head 204`: a status with no body.
fn head_call(node: Node, text: &str, fact: &mut ResponseFact) {
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut args = Vec::new();
    grammar::children(arguments, &mut args);
    if let Some(status) = args
        .first()
        .and_then(|node| symbol_or_int_status(*node, text))
    {
        fact.state(status, WireShape::Unknown);
    }
}

/// The wire shape a rendered value states. Only a literal hash or array is
/// shaped; an ActiveModel, a serializer, or a variable is presence-only.
fn json_shape(node: Node) -> WireShape {
    match node.kind() {
        "hash" => WireShape::Object,
        "array" => WireShape::Array(Box::new(WireShape::Unknown)),
        _ => WireShape::Unknown,
    }
}

/// The status a `:symbol` or an integer states.
fn symbol_or_int_status(node: Node, text: &str) -> Option<u16> {
    let raw = grammar::text(node, text).trim();
    match node.kind() {
        "simple_symbol" => named_status(raw.trim_start_matches(':')),
        "integer" => literal_status(raw),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::grammar::SourceRead;
    use super::super::response_facts::WireShape;
    use super::super::ruby_ast;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-rbtypes-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = ruby_ast::read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    #[test]
    fn a_render_with_a_status_states_it_and_shapes_a_literal() {
        let source = read_source(
            "render-status",
            &[
                ("routes.rb", "post '/v1/blocks', to: 'blocks#create'\n"),
                (
                    "blocks_controller.rb",
                    "class BlocksController < ApplicationController\n\
                     \x20 def create\n\
                     \x20   render json: { id: 1, ok: true }, status: :created\n\
                     \x20 end\nend\n",
                ),
            ],
        );
        let fact = source.responses.get("create").expect("stated");
        assert_eq!(
            fact.statuses[&201],
            WireShape::Object,
            "a literal hash is an object"
        );
    }

    #[test]
    fn an_active_model_render_is_status_only() {
        let source = read_source(
            "presence-only",
            &[
                ("routes.rb", "get '/v1/blocks/:id', to: 'blocks#show'\n"),
                (
                    "blocks_controller.rb",
                    "class BlocksController < ApplicationController\n\
                     \x20 def show\n    render json: @block\n  end\nend\n",
                ),
            ],
        );
        let fact = source.responses.get("show").expect("stated");
        assert_eq!(
            fact.statuses[&200],
            WireShape::Unknown,
            "a model render claims the status, not the shape"
        );
    }

    #[test]
    fn head_no_content_states_204() {
        let source = read_source(
            "head",
            &[
                (
                    "routes.rb",
                    "delete '/v1/blocks/:id', to: 'blocks#destroy'\n",
                ),
                (
                    "blocks_controller.rb",
                    "class BlocksController < ApplicationController\n\
                     \x20 def destroy\n    head :no_content\n  end\nend\n",
                ),
            ],
        );
        let fact = source.responses.get("destroy").expect("stated");
        assert_eq!(fact.statuses[&204], WireShape::Unknown);
    }

    #[test]
    fn a_healthy_action_that_states_no_status_yields_no_claim() {
        let source = read_source(
            "no-claim",
            &[
                ("routes.rb", "get '/v1/ping', to: 'ping#show'\n"),
                (
                    "ping_controller.rb",
                    "class PingController < ApplicationController\n\
                     \x20 def show\n    render plain: 'ok'\n  end\nend\n",
                ),
            ],
        );
        assert!(
            !source.responses.contains_key("show"),
            "an HTML/plain render states no JSON response: {:?}",
            source.responses
        );
    }

    #[test]
    fn an_integer_status_form_is_read() {
        let source = read_source(
            "integer-status",
            &[
                ("routes.rb", "post '/v1/things', to: 'things#create'\n"),
                (
                    "things_controller.rb",
                    "class ThingsController < ApplicationController\n\
                     \x20 def create\n    render json: [1, 2], status: 201\n  end\nend\n",
                ),
            ],
        );
        let fact = source.responses.get("create").expect("stated");
        assert_eq!(
            fact.statuses[&201],
            WireShape::Array(Box::new(WireShape::Unknown)),
            "a literal array is an array of unstated items"
        );
    }
}
