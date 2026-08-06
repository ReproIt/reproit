//! What a Laravel controller states it returns, read from the same parse the
//! route reader walks.
//!
//! Split from `php_ast` at the same boundary as `go_ast`/`go_types`: that file
//! resolves WHERE a request lands, this one resolves WHAT the controller states
//! it writes back. Laravel states a status far more readably than a body:
//! `response()->json($data, 201)`, `abort(404)` and `response()->noContent()`
//! name the code, while an Eloquent model's JSON shape is not statically
//! knowable. So a status is claimed wherever it is stated, and a shape only
//! where a literal array or `compact()` is returned; everything else is
//! presence-only.
//!
//! The route reader records the CLASS as a route's handler, so a controller
//! with two routed actions that state different responses cannot be told apart
//! at emission time. Rather than pick one, such a class abstains, the same rule
//! as an ambiguous type.

use super::field_facts::{drop_ambiguous, record};
use super::grammar::{self};
use super::response_facts::{literal_status, named_status, ResponseFact, WireShape};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

/// Response facts as the file walk collects them, keyed by the controller
/// CLASS, which is the handler the route reader records. Two methods of one
/// class that state different responses make the class ambiguous, because the
/// emitter can carry only one response per class.
#[derive(Default)]
pub(super) struct Responses {
    facts: BTreeMap<String, ResponseFact>,
    ambiguous: BTreeSet<String>,
}

impl Responses {
    /// Read one parsed file: the response facts of every controller method.
    pub(super) fn take_file(&mut self, root: Node, text: &str) {
        grammar::walk(root, &mut |node| {
            if node.kind() == "class_declaration" {
                self.take_class(node, text);
            }
        });
    }

    /// Keep the response facts of routed classes, dropping ambiguous names.
    pub(super) fn finish(
        mut self,
        routes: &[(String, &'static str, Option<String>)],
    ) -> BTreeMap<String, ResponseFact> {
        drop_ambiguous(&mut self.facts, &self.ambiguous);
        let handlers: BTreeSet<&String> = routes
            .iter()
            .filter_map(|(_, _, handler)| handler.as_ref())
            .collect();
        self.facts.retain(|class, _| handlers.contains(class));
        self.facts
    }

    /// Each method of one class that states a response, recorded under the
    /// class name. A method that states nothing does not participate, so a
    /// single stating method wins and two that disagree abstain.
    fn take_class(&mut self, node: Node, text: &str) {
        let Some(name) = grammar::field(node, text, "name") else {
            return;
        };
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let mut members = Vec::new();
        grammar::children(body, &mut members);
        for member in members {
            if member.kind() != "method_declaration" {
                continue;
            }
            if let Some(fact) = method_response(member, text) {
                record(&mut self.facts, &mut self.ambiguous, name.clone(), fact);
            }
        }
    }
}

/// The responses one controller method states: its `response()->json`,
/// `response()->noContent`, `abort`, and its returns. A method that states none
/// of these states nothing.
fn method_response(method: Node, text: &str) -> Option<ResponseFact> {
    let body = method.child_by_field_name("body")?;
    let mut fact = ResponseFact::default();
    grammar::walk(body, &mut |node| match node.kind() {
        "member_call_expression" => member_call(node, text, &mut fact),
        "function_call_expression" => function_call(node, text, &mut fact),
        "return_statement" => return_statement(node, text, &mut fact),
        _ => {}
    });
    (!fact.statuses.is_empty()).then_some(fact)
}

/// `response()->json($data, $status)` and `response()->noContent()`.
fn member_call(node: Node, text: &str, fact: &mut ResponseFact) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    if !receiver_is_response(node, text) {
        return;
    }
    match name.as_str() {
        "json" => {
            let args = arguments_of(node);
            let status = args
                .get(1)
                .and_then(|arg| php_status(*arg, text))
                .unwrap_or(200);
            let body = args
                .first()
                .map(|arg| php_value_shape(*arg, text))
                .unwrap_or(WireShape::Unknown);
            fact.state(status, body);
        }
        "noContent" => fact.state(204, WireShape::Unknown),
        _ => {}
    }
}

/// `abort($status)` and `response($data, $status)`.
fn function_call(node: Node, text: &str, fact: &mut ResponseFact) {
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    let callee = grammar::text(function, text);
    let args = arguments_of(node);
    match callee {
        "abort" => {
            if let Some(status) = args.first().and_then(|arg| php_status(*arg, text)) {
                fact.state(status, WireShape::Unknown);
            }
        }
        // `response($data, $status)` sends the content with a stated status; the
        // body is JSON only when it is a literal array.
        "response" if args.len() >= 2 => {
            if let Some(status) = args.get(1).and_then(|arg| php_status(*arg, text)) {
                fact.state(status, php_value_shape(args[0], text));
            }
        }
        _ => {}
    }
}

/// A `return` in a controller. A literal array or `compact()` is a 200 JSON
/// object; a returned model or collection variable is an implicit 200 JSON of
/// unstated shape. A `return response()->...`, `return view(...)` or
/// `return redirect(...)` states nothing here: the first is read from its own
/// call above, the last two are not JSON.
fn return_statement(node: Node, text: &str, fact: &mut ResponseFact) {
    let Some(value) = node.named_child(0) else {
        return;
    };
    match value.kind() {
        "array_creation_expression" => fact.state(200, array_shape(value, text)),
        "function_call_expression" => {
            if grammar::field(value, text, "function").as_deref() == Some("compact") {
                fact.state(200, WireShape::Object);
            }
        }
        // A bare model or collection variable is Laravel's implicit JSON.
        "variable_name" => fact.state(200, WireShape::Unknown),
        _ => {}
    }
}

/// Whether a member call's receiver is a `response()` call.
fn receiver_is_response(node: Node, text: &str) -> bool {
    let mut cursor = node.child_by_field_name("object");
    while let Some(link) = cursor {
        if link.kind() == "function_call_expression"
            && link
                .child_by_field_name("function")
                .map(|function| grammar::text(function, text))
                == Some("response")
        {
            return true;
        }
        cursor = link.child_by_field_name("object");
    }
    false
}

/// The wire shape a returned value states: a literal array, or `compact()`.
fn php_value_shape(node: Node, text: &str) -> WireShape {
    match node.kind() {
        "array_creation_expression" => array_shape(node, text),
        "function_call_expression"
            if grammar::field(node, text, "function").as_deref() == Some("compact") =>
        {
            WireShape::Object
        }
        _ => WireShape::Unknown,
    }
}

/// A PHP array literal as JSON: an associative array (any `=>` key) is an
/// object, a bare list is an array, an empty array is an object.
fn array_shape(node: Node, text: &str) -> WireShape {
    let mut elements = Vec::new();
    grammar::children(node, &mut elements);
    let mut has_element = false;
    for element in &elements {
        if element.kind() != "array_element_initializer" {
            continue;
        }
        has_element = true;
        // A keyed element carries a `=>`, which the initializer spells with two
        // named children; a bare value has one.
        let mut parts = Vec::new();
        grammar::children(*element, &mut parts);
        if parts.len() >= 2 {
            return WireShape::Object;
        }
    }
    let _ = text;
    if has_element {
        WireShape::Array(Box::new(WireShape::Unknown))
    } else {
        WireShape::Object
    }
}

/// The status an argument states: an integer literal or an `HTTP_xxx`
/// constant (`Response::HTTP_CREATED`). Anything else states no status.
fn php_status(node: Node, text: &str) -> Option<u16> {
    let raw = grammar::text(node, text).trim();
    if let Some(code) = literal_status(raw) {
        return Some(code);
    }
    let tail = raw.rsplit("::").next().unwrap_or(raw);
    named_status(tail.trim_start_matches("HTTP_"))
}

fn arguments_of(node: Node) -> Vec<Node> {
    let mut args = Vec::new();
    if let Some(arguments) = node.child_by_field_name("arguments") {
        let mut cursor = arguments.walk();
        args.extend(
            arguments
                .children(&mut cursor)
                .filter(|child| child.kind() == "argument"),
        );
    }
    // An `argument` wraps its expression; unwrap to the value the readers match.
    args.into_iter()
        .filter_map(|argument| argument.named_child(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::grammar::SourceRead;
    use super::super::php_ast;
    use super::super::response_facts::WireShape;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-phptypes-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = php_ast::read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    #[test]
    fn a_json_response_states_its_status_and_a_literal_shape() {
        let source = read_source(
            "json",
            &[
                (
                    "routes.php",
                    "<?php\nRoute::post('/blocks', [BlockController::class, 'store']);\n",
                ),
                (
                    "BlockController.php",
                    "<?php\nclass BlockController extends Controller {\n\
                     \x20 public function store() {\n\
                     \x20   return response()->json(['id' => 1, 'ok' => true], 201);\n\
                     \x20 }\n}\n",
                ),
            ],
        );
        let fact = source.responses.get("BlockController").expect("stated");
        assert_eq!(
            fact.statuses[&201],
            WireShape::Object,
            "an associative array is an object"
        );
    }

    #[test]
    fn a_model_return_and_abort_state_status_only() {
        let source = read_source(
            "model-and-abort",
            &[
                (
                    "routes.php",
                    "<?php\nRoute::get('/blocks/{id}', [BlockController::class, 'show']);\n",
                ),
                (
                    "BlockController.php",
                    "<?php\nclass BlockController extends Controller {\n\
                     \x20 public function show($id) {\n\
                     \x20   $block = Block::find($id);\n\
                     \x20   if (!$block) { abort(404); }\n\
                     \x20   return $block;\n\
                     \x20 }\n}\n",
                ),
            ],
        );
        let fact = source.responses.get("BlockController").expect("stated");
        assert_eq!(
            fact.statuses[&404],
            WireShape::Unknown,
            "a stated abort status"
        );
        assert_eq!(
            fact.statuses[&200],
            WireShape::Unknown,
            "an Eloquent model return claims the status, not the shape"
        );
    }

    #[test]
    fn no_content_states_204() {
        let source = read_source(
            "nocontent",
            &[
                (
                    "routes.php",
                    "<?php\nRoute::delete('/blocks/{id}', [BlockController::class, 'destroy']);\n",
                ),
                (
                    "BlockController.php",
                    "<?php\nclass BlockController extends Controller {\n\
                     \x20 public function destroy($id) {\n    return response()->noContent();\n  }\n}\n",
                ),
            ],
        );
        let fact = source.responses.get("BlockController").expect("stated");
        assert_eq!(fact.statuses[&204], WireShape::Unknown);
    }

    #[test]
    fn two_actions_that_disagree_make_the_class_abstain() {
        // The route reader keys a handler by class, so the emitter can carry one
        // response per class. Two methods stating different responses cannot be
        // told apart, so the class abstains rather than pick one.
        let source = read_source(
            "ambiguous-class",
            &[
                (
                    "routes.php",
                    "<?php\nRoute::get('/x', [C::class, 'index']);\n\
                     Route::post('/x', [C::class, 'store']);\n",
                ),
                (
                    "C.php",
                    "<?php\nclass C extends Controller {\n\
                     \x20 public function index() { return response()->json([1, 2], 200); }\n\
                     \x20 public function store() { return response()->json(['a' => 1], 201); }\n}\n",
                ),
            ],
        );
        assert!(
            !source.responses.contains_key("C"),
            "a class with disagreeing actions must abstain: {:?}",
            source.responses.get("C")
        );
    }

    #[test]
    fn a_view_return_states_no_json_response() {
        let source = read_source(
            "view",
            &[
                (
                    "routes.php",
                    "<?php\nRoute::get('/home', [PageController::class, 'home']);\n",
                ),
                (
                    "PageController.php",
                    "<?php\nclass PageController extends Controller {\n\
                     \x20 public function home() { return view('home'); }\n}\n",
                ),
            ],
        );
        assert!(
            !source.responses.contains_key("PageController"),
            "a view render is not a JSON response: {:?}",
            source.responses
        );
    }

    #[test]
    fn a_literal_list_is_an_array() {
        let source = read_source(
            "list",
            &[
                (
                    "routes.php",
                    "<?php\nRoute::get('/ids', [IdController::class, 'index']);\n",
                ),
                (
                    "IdController.php",
                    "<?php\nclass IdController extends Controller {\n\
                     \x20 public function index() { return response()->json([1, 2, 3]); }\n}\n",
                ),
            ],
        );
        let fact = source.responses.get("IdController").expect("stated");
        assert_eq!(
            fact.statuses[&200],
            WireShape::Array(Box::new(WireShape::Unknown)),
            "a bare list defaults to 200 and is an array"
        );
    }
}
