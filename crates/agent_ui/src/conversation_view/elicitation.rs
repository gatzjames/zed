use acp_thread::{Elicitation, ElicitationEntryId, ElicitationStatus};
use agent_client_protocol::schema::v2 as acp;
use collections::{HashMap, HashSet};
use component::{Component, ComponentScope, example_group_with_title, single_example};
use editor::Editor;
use futures::channel::oneshot;
use gpui::{AnyElement, App, Div, Empty, Entity, Focusable, Role, SharedString, Window, div};
use std::collections::BTreeMap;
use std::rc::Rc;
use ui::{
    Button, Checkbox, ChoiceCard, Color, Icon, IconName, IconSize, Label, LabelSize, ToggleState,
    prelude::*,
};

#[derive(Clone)]
struct ElicitationOption {
    value: String,
    label: SharedString,
    description: Option<SharedString>,
}

enum ElicitationFieldState {
    Text(Entity<Editor>),
    Boolean(bool),
    SingleSelect { value: Option<String> },
    MultiSelect(HashSet<String>),
}

#[derive(PartialEq, Eq)]
enum ElicitationFieldValue {
    Text(String),
    Boolean(bool),
    SingleSelect { value: Option<String> },
    MultiSelect(HashSet<String>),
}

#[derive(PartialEq, Eq)]
pub(crate) struct ElicitationFormSubmission {
    fields: HashMap<String, ElicitationFieldValue>,
}

pub(crate) struct ElicitationFormState {
    fields: HashMap<String, ElicitationFieldState>,
    field_errors: HashMap<String, SharedString>,
    is_submitting: bool,
}

impl ElicitationFormState {
    pub(crate) fn new(schema: &acp::ElicitationSchema, window: &mut Window, cx: &mut App) -> Self {
        let required = schema.required.as_deref().unwrap_or_default();
        let mut fields = HashMap::default();

        for (name, property) in &schema.properties {
            let is_required = required.iter().any(|required| required == name);
            let field = match property {
                acp::ElicitationPropertySchema::String(schema) => {
                    let options = single_select_options(schema);
                    if options.is_empty() {
                        let editor = cx.new(|cx| {
                            let mut editor = Editor::single_line(window, cx);
                            if let Some(default) = &schema.default {
                                editor.set_text(default.clone(), window, cx);
                            }
                            editor
                        });
                        ElicitationFieldState::Text(editor)
                    } else {
                        let value = single_select_default_value(schema, &options).or_else(|| {
                            is_required
                                .then(|| options.first().map(|option| option.value.clone()))
                                .flatten()
                        });
                        ElicitationFieldState::SingleSelect { value }
                    }
                }
                acp::ElicitationPropertySchema::Number(schema) => {
                    let editor = cx.new(|cx| {
                        let mut editor = Editor::single_line(window, cx);
                        if let Some(default) = schema.default {
                            editor.set_text(default.to_string(), window, cx);
                        }
                        editor
                    });
                    ElicitationFieldState::Text(editor)
                }
                acp::ElicitationPropertySchema::Integer(schema) => {
                    let editor = cx.new(|cx| {
                        let mut editor = Editor::single_line(window, cx);
                        if let Some(default) = schema.default {
                            editor.set_text(default.to_string(), window, cx);
                        }
                        editor
                    });
                    ElicitationFieldState::Text(editor)
                }
                acp::ElicitationPropertySchema::Boolean(schema) => {
                    ElicitationFieldState::Boolean(schema.default.unwrap_or(false))
                }
                acp::ElicitationPropertySchema::Array(schema) => {
                    ElicitationFieldState::MultiSelect(
                        schema
                            .default
                            .clone()
                            .unwrap_or_default()
                            .into_iter()
                            .collect(),
                    )
                }
                _ => continue,
            };
            fields.insert(name.clone(), field);
        }

        Self {
            fields,
            field_errors: HashMap::default(),
            is_submitting: false,
        }
    }

    fn snapshot(&self, cx: &App) -> ElicitationFormSubmission {
        ElicitationFormSubmission {
            fields: self
                .fields
                .iter()
                .map(|(name, field)| {
                    let value = match field {
                        ElicitationFieldState::Text(editor) => {
                            ElicitationFieldValue::Text(editor.read(cx).text(cx))
                        }
                        ElicitationFieldState::Boolean(value) => {
                            ElicitationFieldValue::Boolean(*value)
                        }
                        ElicitationFieldState::SingleSelect { value } => {
                            ElicitationFieldValue::SingleSelect {
                                value: value.clone(),
                            }
                        }
                        ElicitationFieldState::MultiSelect(values) => {
                            ElicitationFieldValue::MultiSelect(values.clone())
                        }
                    };
                    (name.clone(), value)
                })
                .collect(),
        }
    }

    pub(crate) fn begin_submission(&mut self, cx: &App) -> Option<ElicitationFormSubmission> {
        if self.is_submitting {
            return None;
        }
        self.is_submitting = true;
        Some(self.snapshot(cx))
    }

    pub(crate) fn validation_matches_current_values(
        &mut self,
        submission: &ElicitationFormSubmission,
        cx: &App,
    ) -> bool {
        let is_current = self.snapshot(cx) == *submission;
        if !is_current {
            self.is_submitting = false;
        }
        is_current
    }

    #[cfg(test)]
    pub(crate) fn collect(
        &self,
        schema: &acp::ElicitationSchema,
        cx: &App,
    ) -> Result<BTreeMap<String, acp::ElicitationContentValue>, HashMap<String, SharedString>> {
        self.snapshot(cx).validate(schema)
    }

    fn field_has_value(&self, field_name: &str, cx: &App) -> bool {
        match self.fields.get(field_name) {
            Some(ElicitationFieldState::Text(editor)) => {
                !editor.read(cx).text(cx).trim().is_empty()
            }
            Some(ElicitationFieldState::Boolean(_)) => true,
            Some(ElicitationFieldState::SingleSelect { value }) => value.is_some(),
            Some(ElicitationFieldState::MultiSelect(values)) => !values.is_empty(),
            None => false,
        }
    }

    pub(crate) fn is_answered(&self, field_names: &[String], cx: &App) -> bool {
        field_names
            .iter()
            .any(|field_name| self.field_has_value(field_name, cx))
    }

    pub(crate) fn has_errors(&self, field_names: &[String]) -> bool {
        field_names
            .iter()
            .any(|field_name| self.field_errors.contains_key(field_name))
    }

    /// A one-line summary of the answers given in these fields.
    pub(crate) fn answer_summary(&self, field_names: &[String], cx: &App) -> Option<String> {
        let answers = field_names
            .iter()
            .filter_map(|field_name| match self.fields.get(field_name)? {
                ElicitationFieldState::Text(editor) => {
                    Some(editor.read(cx).text(cx).trim().to_string())
                        .filter(|text| !text.is_empty())
                }
                ElicitationFieldState::Boolean(value) => {
                    Some(if *value { "Yes" } else { "No" }.to_string())
                }
                ElicitationFieldState::SingleSelect { value } => value.clone(),
                ElicitationFieldState::MultiSelect(values) => {
                    let mut values = values.iter().cloned().collect::<Vec<_>>();
                    values.sort();
                    Some(values.join(", ")).filter(|values| !values.is_empty())
                }
            })
            .collect::<Vec<_>>();
        (!answers.is_empty()).then(|| answers.join(" · "))
    }

    pub(crate) fn set_errors(&mut self, errors: HashMap<String, SharedString>) {
        self.field_errors = errors;
        self.is_submitting = false;
    }

    pub(crate) fn set_field_error(
        &mut self,
        field_name: impl Into<String>,
        error: impl Into<SharedString>,
    ) {
        self.field_errors.insert(field_name.into(), error.into());
    }

    pub(crate) fn set_boolean(&mut self, field_name: &str, value: bool) {
        if let Some(ElicitationFieldState::Boolean(field)) = self.fields.get_mut(field_name) {
            *field = value;
            self.field_errors.remove(field_name);
        }
    }

    pub(crate) fn set_single_select(&mut self, field_name: &str, value: String) {
        if let Some(ElicitationFieldState::SingleSelect { value: selected }) =
            self.fields.get_mut(field_name)
        {
            *selected = Some(value);
            self.field_errors.remove(field_name);
        }
    }

    pub(crate) fn set_multi_select(&mut self, field_name: &str, value: String, selected: bool) {
        if let Some(ElicitationFieldState::MultiSelect(values)) = self.fields.get_mut(field_name) {
            if selected {
                values.insert(value);
            } else {
                values.remove(&value);
            }
            self.field_errors.remove(field_name);
        }
    }
}

impl ElicitationFormSubmission {
    pub(crate) fn validate(
        &self,
        schema: &acp::ElicitationSchema,
    ) -> Result<BTreeMap<String, acp::ElicitationContentValue>, HashMap<String, SharedString>> {
        let required = schema.required.as_deref().unwrap_or_default();
        let mut content = BTreeMap::new();
        let mut errors = HashMap::default();

        for (name, property) in &schema.properties {
            let is_required = required.iter().any(|required| required == name);
            let Some(field) = self.fields.get(name) else {
                continue;
            };

            let field_content = match (property, field) {
                (
                    acp::ElicitationPropertySchema::String(schema),
                    ElicitationFieldValue::Text(value),
                ) => {
                    if value.is_empty() {
                        if is_required {
                            Err(format!("{} is required", property_title(name, property)).into())
                        } else {
                            Ok(None)
                        }
                    } else {
                        validate_string_value(property_title(name, property), schema, value)
                            .map(|()| Some(value.clone().into()))
                    }
                }
                (
                    acp::ElicitationPropertySchema::String(schema),
                    ElicitationFieldValue::SingleSelect { value },
                ) => {
                    if let Some(value) = value {
                        validate_single_select_value(property_title(name, property), schema, value)
                            .and_then(|()| {
                                validate_string_value(property_title(name, property), schema, value)
                            })
                            .map(|()| Some(value.clone().into()))
                    } else if is_required {
                        Err(format!("{} is required", property_title(name, property)).into())
                    } else {
                        Ok(None)
                    }
                }
                (
                    acp::ElicitationPropertySchema::Number(schema),
                    ElicitationFieldValue::Text(value),
                ) => {
                    let value = value.trim();
                    if value.is_empty() {
                        if is_required {
                            Err(format!("{} is required", property_title(name, property)).into())
                        } else {
                            Ok(None)
                        }
                    } else {
                        validate_number_value(property_title(name, property), schema, value)
                            .map(|parsed| Some(parsed.into()))
                    }
                }
                (
                    acp::ElicitationPropertySchema::Integer(schema),
                    ElicitationFieldValue::Text(value),
                ) => {
                    let value = value.trim();
                    if value.is_empty() {
                        if is_required {
                            Err(format!("{} is required", property_title(name, property)).into())
                        } else {
                            Ok(None)
                        }
                    } else {
                        validate_integer_value(property_title(name, property), schema, value)
                            .map(|parsed| Some(parsed.into()))
                    }
                }
                (
                    acp::ElicitationPropertySchema::Boolean(schema),
                    ElicitationFieldValue::Boolean(value),
                ) => {
                    if is_required || *value || schema.default.is_some() {
                        Ok(Some((*value).into()))
                    } else {
                        Ok(None)
                    }
                }
                (
                    acp::ElicitationPropertySchema::Array(schema),
                    ElicitationFieldValue::MultiSelect(selected),
                ) => {
                    let mut values = multi_select_options(schema)
                        .into_iter()
                        .filter_map(|option| {
                            selected.contains(&option.value).then_some(option.value)
                        })
                        .collect::<Vec<_>>();
                    values.sort();
                    if values.is_empty() && !is_required {
                        Ok(None)
                    } else if schema
                        .min_items
                        .is_some_and(|min_items| values.len() < min_items as usize)
                    {
                        Err(
                            format!("{} needs more selections", property_title(name, property))
                                .into(),
                        )
                    } else if schema
                        .max_items
                        .is_some_and(|max_items| values.len() > max_items as usize)
                    {
                        Err(
                            format!("{} has too many selections", property_title(name, property))
                                .into(),
                        )
                    } else {
                        Ok(Some(values.into()))
                    }
                }
                _ => Ok(None),
            };

            match field_content {
                Ok(Some(value)) => {
                    content.insert(name.clone(), value);
                }
                Ok(None) => {}
                Err(error) => {
                    errors.insert(name.clone(), error);
                }
            }
        }

        if errors.is_empty() {
            Ok(content)
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{KeyUpEvent, Keystroke, PlatformInput, TestAppContext, VisualTestContext};
    use std::cell::RefCell;

    struct TestElicitationView {
        elicitation: Elicitation,
        form_state: ElicitationFormState,
        events: Rc<RefCell<Vec<String>>>,
    }

    impl TestElicitationView {
        fn new(schema: acp::ElicitationSchema, window: &mut Window, cx: &mut App) -> Self {
            Self {
                form_state: ElicitationFormState::new(&schema, window, cx),
                elicitation: Elicitation {
                    id: ElicitationEntryId("keyboard-test".into()),
                    request: acp::CreateElicitationRequest::new(
                        acp::ElicitationFormMode::new(preview_request_scope(0), schema),
                        "Choose an answer.",
                    ),
                    status: pending_status(),
                },
                events: Rc::default(),
            }
        }

        fn editor(&self, field_name: &str) -> Entity<Editor> {
            match self.form_state.fields.get(field_name) {
                Some(ElicitationFieldState::Text(editor)) => editor.clone(),
                _ => panic!("expected a text field named {field_name}"),
            }
        }
    }

    impl Render for TestElicitationView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let response_handler = |response: &'static str| -> RespondHandler {
                let events = self.events.clone();
                let expected_id = self.elicitation.id.clone();
                Rc::new(move |id, _, _| {
                    assert_eq!(id, expected_id);
                    events.borrow_mut().push(response.into());
                })
            };
            let handlers = ElicitationCardHandlers {
                on_submit: response_handler("submit"),
                on_decline: response_handler("decline"),
                on_cancel: response_handler("cancel"),
                on_dismiss_url: response_handler("dismiss"),
                on_open_url: {
                    let events = self.events.clone();
                    let expected_id = self.elicitation.id.clone();
                    Rc::new(move |id, url, _, _| {
                        assert_eq!(id, expected_id);
                        events.borrow_mut().push(format!("open: {url}"));
                    })
                },
                on_boolean_change: {
                    let events = self.events.clone();
                    Rc::new(move |_, name, value, _| {
                        events.borrow_mut().push(format!("{name}: {value}"));
                    })
                },
                on_single_select_change: {
                    let events = self.events.clone();
                    Rc::new(move |_, name, value, _| {
                        events.borrow_mut().push(format!("{name}: {value}"));
                    })
                },
                on_multi_select_change: {
                    let events = self.events.clone();
                    Rc::new(move |_, name, value, selected, _| {
                        events
                            .borrow_mut()
                            .push(format!("{name}: {value} = {selected}"));
                    })
                },
            };

            div()
                .key_context("AcpThread")
                .on_action(|_: &crate::CycleModeSelector, _, _| {
                    panic!("form navigation must not cycle the agent mode");
                })
                .on_action(|_: &zed_actions::agent::Chat, _, _| {
                    panic!("form submission must not send a chat message");
                })
                .child(
                    ElicitationCard::new(
                        0,
                        &self.elicitation,
                        "Claude Code".into(),
                        Some(&self.form_state),
                        handlers,
                    )
                    .render(cx),
                )
        }
    }

    fn init_keyboard_test(cx: &mut TestAppContext) {
        crate::conversation_view::tests::init_test(cx);
        cx.update(|cx| {
            let bindings = settings::KeymapFile::load_asset_allow_partial_failure(
                "keymaps/default-linux.json",
                cx,
            )
            .expect("default keymap should load");
            cx.bind_keys(bindings);
        });
    }

    fn press_keys(cx: &mut VisualTestContext, keys: &str) {
        for key in keys.split_whitespace() {
            cx.simulate_keystrokes(key);
            cx.update(|window, cx| {
                // Click handlers activate buttons on key release, not key press.
                window.dispatch_event(
                    PlatformInput::KeyUp(KeyUpEvent {
                        keystroke: Keystroke::parse(key).expect("valid test keystroke"),
                    }),
                    cx,
                );
            });
            cx.run_until_parked();
        }
    }

    fn question_schema(questions: &[(&str, &str, bool)]) -> acp::ElicitationSchema {
        let mut schema = acp::ElicitationSchema::new();
        for (index, (header, question, multi_select)) in questions.iter().enumerate() {
            let options = vec![
                acp::EnumOption::new("first", "First"),
                acp::EnumOption::new("second", "Second"),
            ];
            let field_name = format!("question_{index}");
            schema = if *multi_select {
                schema.property(
                    field_name.clone(),
                    acp::MultiSelectPropertySchema::titled(options)
                        .title(*header)
                        .description(*question),
                    false,
                )
            } else {
                schema.property(
                    field_name.clone(),
                    acp::StringPropertySchema::new()
                        .one_of(options)
                        .title(*header)
                        .description(*question),
                    false,
                )
            };
            schema = schema.property(
                format!("{field_name}_custom"),
                acp::StringPropertySchema::new().title("Other"),
                false,
            );
        }
        schema
    }

    fn form_elicitation(id: &str, message: &str, schema: acp::ElicitationSchema) -> Elicitation {
        Elicitation {
            id: ElicitationEntryId(id.into()),
            request: acp::CreateElicitationRequest::new(
                acp::ElicitationFormMode::new(preview_request_scope(0), schema),
                message,
            ),
            status: pending_status(),
        }
    }

    #[test]
    fn question_tabs_split_question_lists_and_keep_other_forms_whole() {
        let questions = form_elicitation(
            "questions",
            "Please answer the following questions.",
            question_schema(&[
                ("Auth", "Which auth?", false),
                ("Storage", "Which storage?", true),
            ]),
        );
        let lone_question = form_elicitation(
            "lone",
            "Which backend?",
            acp::ElicitationSchema::new().property(
                "question_0",
                acp::StringPropertySchema::new()
                    .one_of(vec![acp::EnumOption::new("sqlite", "SQLite")])
                    .title("Backend"),
                false,
            ),
        );
        let mixed_form = form_elicitation(
            "mixed",
            "Configure the server",
            acp::ElicitationSchema::new().string("name", true).property(
                "region",
                acp::StringPropertySchema::new().one_of(vec![acp::EnumOption::new("eu", "Europe")]),
                true,
            ),
        );

        let tabs = question_tabs(&[&questions, &lone_question, &mixed_form]);
        assert_eq!(
            tabs,
            vec![
                QuestionTab {
                    question_ix: 0,
                    field_names: vec!["question_0".into(), "question_0_custom".into()],
                    label: "Auth".into(),
                    prompt: None,
                },
                QuestionTab {
                    question_ix: 0,
                    field_names: vec!["question_1".into(), "question_1_custom".into()],
                    label: "Storage".into(),
                    prompt: None,
                },
                QuestionTab {
                    question_ix: 1,
                    field_names: vec!["question_0".into()],
                    label: "Backend".into(),
                    prompt: Some("Which backend?".into()),
                },
                QuestionTab {
                    question_ix: 2,
                    field_names: vec!["name".into(), "region".into()],
                    label: "Configure the server".into(),
                    prompt: Some("Configure the server".into()),
                },
            ]
        );
    }

    struct TestQuestionsView {
        questions: Vec<(Elicitation, ElicitationFormState)>,
        selected_tab: usize,
        events: Rc<RefCell<Vec<String>>>,
    }

    impl Render for TestQuestionsView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let events = self.events.clone();
            let record = move |event: String| events.borrow_mut().push(event);
            let field_handlers = {
                let mut handlers = ElicitationCardHandlers::noop();
                let record = record.clone();
                handlers.on_single_select_change =
                    Rc::new(move |_, name, value, _| record(format!("{name}: {value}")));
                handlers
            };
            let respond = |response: &'static str| -> QuestionsHandler {
                let record = record.clone();
                Rc::new(move |_, _| record(response.to_string()))
            };
            let handlers = QuestionsCardHandlers {
                on_select_tab: {
                    let record = record.clone();
                    Rc::new(move |tab, _| record(format!("tab: {tab}")))
                },
                on_submit_all: respond("submit"),
                on_decline_all: respond("decline"),
                on_cancel_all: respond("cancel"),
            };
            div().size_full().child(
                QuestionsCard::new(
                    self.questions
                        .iter()
                        .enumerate()
                        .map(|(entry_ix, (elicitation, form_state))| QuestionForm {
                            entry_ix,
                            elicitation,
                            form_state: Some(form_state),
                        })
                        .collect(),
                    "Claude Code".into(),
                    self.selected_tab,
                    field_handlers,
                    handlers,
                )
                .render(cx),
            )
        }
    }

    #[gpui::test]
    fn questions_card_shows_one_question_per_tab(cx: &mut TestAppContext) {
        init_keyboard_test(cx);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let schema = question_schema(&[
                ("Auth", "Which auth?", false),
                ("Storage", "Which storage?", false),
            ]);
            TestQuestionsView {
                questions: vec![(
                    form_elicitation("questions", "Please answer.", schema.clone()),
                    ElicitationFormState::new(&schema, window, cx),
                )],
                selected_tab: 0,
                events: Rc::default(),
            }
        });
        let events = view.read_with(cx, |view, _| view.events.clone());
        cx.run_until_parked();
        for tab in ["question-tab-0", "question-tab-1", "question-tab-2"] {
            assert!(cx.debug_bounds(tab).is_some(), "missing {tab}");
        }
        assert!(cx.debug_bounds("questions-summary").is_none());

        cx.update(|window, cx| window.focus_next(cx));
        press_keys(cx, "2");
        assert_eq!(
            *events.borrow(),
            ["question_0: second", "tab: 1"],
            "picking an option by number moves on to the next question"
        );
        events.borrow_mut().clear();
        press_keys(cx, "right");
        assert_eq!(*events.borrow(), ["tab: 1"]);

        view.update(cx, |view, cx| {
            view.selected_tab = 2;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("questions-summary").is_some());
    }

    #[gpui::test]
    fn questions_card_without_several_questions_has_no_tabs(cx: &mut TestAppContext) {
        init_keyboard_test(cx);
        let (_view, cx) = cx.add_window_view(|window, cx| {
            let schema = question_schema(&[("Auth", "Which auth?", false)]);
            TestQuestionsView {
                questions: vec![(
                    form_elicitation("question", "Which auth?", schema.clone()),
                    ElicitationFormState::new(&schema, window, cx),
                )],
                selected_tab: 0,
                events: Rc::default(),
            }
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("question-tab-0").is_none());
    }

    #[gpui::test]
    fn form_text_fields_submit_on_enter(cx: &mut TestAppContext) {
        init_keyboard_test(cx);
        let (view, cx) = cx.add_window_view(|window, cx| {
            TestElicitationView::new(
                acp::ElicitationSchema::new()
                    .string("other", true)
                    .property("count", acp::IntegerPropertySchema::new(), true)
                    .property("amount", acp::NumberPropertySchema::new(), true),
                window,
                cx,
            )
        });
        let events = view.read_with(cx, |view, _| view.events.clone());

        for (field_name, value) in [
            ("other", "  My answer  "),
            ("count", "2"),
            ("amount", "1.5"),
        ] {
            let editor = view.read_with(cx, |view, _| view.editor(field_name));
            cx.update(|window, cx| window.focus(&editor.focus_handle(cx), cx));
            cx.simulate_input(value);
            press_keys(cx, "enter");
            assert_eq!(editor.read_with(cx, |editor, cx| editor.text(cx)), value);
        }
        assert_eq!(*events.borrow(), ["submit", "submit", "submit"]);

        view.update(cx, |view, cx| {
            assert!(view.form_state.begin_submission(cx).is_some());
            cx.notify();
        });
        press_keys(cx, "enter");
        assert_eq!(events.borrow().len(), 3);
    }

    #[gpui::test]
    fn form_tab_navigation_reaches_fields_and_actions(cx: &mut TestAppContext) {
        init_keyboard_test(cx);
        let (view, cx) = cx.add_window_view(|window, cx| {
            TestElicitationView::new(
                acp::ElicitationSchema::new()
                    .string("first", false)
                    .string("other", false),
                window,
                cx,
            )
        });
        let (first, other, events) = view.read_with(cx, |view, _| {
            (
                view.editor("first"),
                view.editor("other"),
                view.events.clone(),
            )
        });
        cx.update(|window, cx| window.focus(&first.focus_handle(cx), cx));

        press_keys(cx, "tab");
        cx.update(|window, cx| assert!(other.focus_handle(cx).is_focused(window)));
        press_keys(cx, "shift-tab");
        cx.update(|window, cx| assert!(first.focus_handle(cx).is_focused(window)));

        press_keys(cx, "tab tab");
        let submit_focus =
            cx.update(|window, cx| window.focused(cx).expect("Submit should be focused"));
        press_keys(cx, "shift-tab");
        cx.update(|window, cx| assert!(other.focus_handle(cx).is_focused(window)));
        press_keys(cx, "tab");
        cx.update(|window, _| assert!(submit_focus.is_focused(window)));
        press_keys(cx, "enter tab enter tab space");
        assert_eq!(*events.borrow(), ["submit", "decline", "cancel"]);

        press_keys(cx, "shift-tab space");
        assert_eq!(*events.borrow(), ["submit", "decline", "cancel", "decline"]);
        let decline_focus =
            cx.update(|window, cx| window.focused(cx).expect("Decline should be focused"));

        cx.update(|window, cx| window.focus(&submit_focus, cx));
        view.update(cx, |view, cx| {
            assert!(view.form_state.begin_submission(cx).is_some());
            cx.notify();
        });
        press_keys(cx, "tab");
        cx.update(|window, _| assert!(decline_focus.is_focused(window)));
        press_keys(cx, "shift-tab");
        cx.update(|window, cx| assert!(other.focus_handle(cx).is_focused(window)));
        press_keys(cx, "tab");
        cx.update(|window, _| assert!(decline_focus.is_focused(window)));
        press_keys(cx, "shift-tab");
        cx.update(|window, cx| assert!(other.focus_handle(cx).is_focused(window)));

        view.update(cx, |view, cx| {
            view.form_state.set_errors(HashMap::default());
            cx.notify();
        });
        press_keys(cx, "tab");
        cx.update(|window, _| assert!(submit_focus.is_focused(window)));
    }

    #[gpui::test]
    fn form_choices_are_keyboard_accessible(cx: &mut TestAppContext) {
        init_keyboard_test(cx);
        let (view, cx) = cx.add_window_view(|window, cx| {
            TestElicitationView::new(
                acp::ElicitationSchema::new()
                    .string("answer", false)
                    .property("boolean", acp::BooleanPropertySchema::new(), false)
                    .property(
                        "choice",
                        acp::StringPropertySchema::new()
                            .enum_values(vec!["first".into(), "second".into()]),
                        false,
                    )
                    .property(
                        "multiple",
                        acp::MultiSelectPropertySchema::new(vec!["first".into(), "second".into()]),
                        false,
                    ),
                window,
                cx,
            )
        });
        let (editor, events) =
            view.read_with(cx, |view, _| (view.editor("answer"), view.events.clone()));
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx), cx));
        press_keys(
            cx,
            "tab space tab enter tab space tab space tab enter tab enter",
        );
        assert_eq!(
            *events.borrow(),
            [
                "boolean: true",
                "choice: first",
                "choice: second",
                "multiple: first = true",
                "multiple: second = true",
                "submit",
            ]
        );
    }

    #[gpui::test]
    fn tab_navigation_between_form_and_url_cards(cx: &mut TestAppContext) {
        struct Cards {
            form: Entity<TestElicitationView>,
            url: Entity<TestElicitationView>,
        }

        impl Render for Cards {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                v_flex().child(self.form.clone()).child(self.url.clone())
            }
        }

        init_keyboard_test(cx);
        let (cards, cx) = cx.add_window_view(|window, cx| Cards {
            form: cx.new(|cx| {
                TestElicitationView::new(
                    acp::ElicitationSchema::new().string("other", false),
                    window,
                    cx,
                )
            }),
            url: cx.new(|cx| {
                let mut view = TestElicitationView::new(acp::ElicitationSchema::new(), window, cx);
                view.elicitation.id = ElicitationEntryId("url-test".into());
                view.elicitation.request = acp::CreateElicitationRequest::new(
                    acp::ElicitationUrlMode::new(
                        preview_request_scope(1),
                        acp::ElicitationId::new("url-test"),
                        "https://example.com/authorize",
                    ),
                    "Authorize access.",
                );
                view
            }),
        });
        let (form, url) = cards.read_with(cx, |cards, _| (cards.form.clone(), cards.url.clone()));
        let (editor, form_events) =
            form.read_with(cx, |form, _| (form.editor("other"), form.events.clone()));
        let url_events = url.read_with(cx, |url, _| url.events.clone());
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx), cx));

        press_keys(cx, "tab tab tab enter");
        assert_eq!(*form_events.borrow(), ["cancel"]);
        press_keys(cx, "tab enter");
        assert_eq!(
            *url_events.borrow(),
            ["open: https://example.com/authorize", "submit"]
        );

        press_keys(cx, "shift-tab enter");
        assert_eq!(*form_events.borrow(), ["cancel", "cancel"]);
        press_keys(cx, "shift-tab shift-tab enter");
        assert_eq!(*form_events.borrow(), ["cancel", "cancel", "submit"]);
        press_keys(cx, "shift-tab");
        cx.update(|window, cx| assert!(editor.focus_handle(cx).is_focused(window)));

        url.update(cx, |url, cx| {
            url.elicitation.status = ElicitationStatus::Accepted;
            cx.notify();
        });
        press_keys(cx, "tab tab tab tab enter tab enter");
        assert_eq!(
            *url_events.borrow(),
            [
                "open: https://example.com/authorize",
                "submit",
                "open: https://example.com/authorize",
                "dismiss",
            ]
        );
        assert_eq!(*form_events.borrow(), ["cancel", "cancel", "submit"]);
    }

    #[test]
    fn string_validation_rejects_email_format_mismatch() {
        let schema = acp::StringPropertySchema::email();

        validate_string_value("Email".into(), &schema, "user@example.com")
            .expect("valid email should be accepted");
        assert_eq!(
            validate_string_value("Email".into(), &schema, "not-an-email")
                .expect_err("invalid email should be rejected")
                .to_string(),
            "Email must be an email address"
        );

        let format = acp::StringFormat::Other("future-email".into());
        assert_eq!(string_format_json_name(&format), None);
        assert_eq!(string_format_label(&format), None);
        let schema = acp::StringPropertySchema::new().format(format);
        validate_string_value("Email".into(), &schema, "not-an-email")
            .expect("unknown formats should be annotations");

        let schema = schema.pattern("^prod-[0-9]+$");
        validate_string_value("Email".into(), &schema, "prod-42")
            .expect("known constraints should still apply alongside unknown formats");
        assert_eq!(
            validate_string_value("Email".into(), &schema, "not-an-email")
                .expect_err("unknown formats should not bypass known constraints")
                .to_string(),
            "Email does not match the requested pattern"
        );
    }

    #[test]
    fn string_validation_rejects_pattern_mismatch() {
        let schema = acp::StringPropertySchema::new().pattern("^prod-[0-9]+$");

        validate_string_value("Environment".into(), &schema, "prod-42")
            .expect("matching pattern should be accepted");
        assert_eq!(
            validate_string_value("Environment".into(), &schema, "dev-42")
                .expect_err("pattern mismatch should be rejected")
                .to_string(),
            "Environment does not match the requested pattern"
        );
    }

    #[test]
    fn string_validation_supports_bounded_advanced_patterns() {
        let schema = acp::StringPropertySchema::new().pattern(r"^prod-(?=[0-9]+$)([0-9])\1$");

        validate_string_value("Environment".into(), &schema, "prod-44")
            .expect("lookahead and backreference should be accepted");
        assert_eq!(
            validate_string_value("Environment".into(), &schema, "prod-45")
                .expect_err("backreference mismatch should be rejected")
                .to_string(),
            "Environment does not match the requested pattern"
        );
    }

    #[test]
    fn string_validation_rejects_patterns_outside_resource_limits() {
        let oversized_pattern =
            acp::StringPropertySchema::new().pattern("a".repeat(MAX_ELICITATION_PATTERN_BYTES + 1));
        assert_eq!(
            validate_string_value("Value".into(), &oversized_pattern, "a")
                .expect_err("oversized pattern should be rejected")
                .to_string(),
            "Value has an invalid validation pattern"
        );

        let schema = acp::StringPropertySchema::new().pattern(".*");
        let oversized_value = "a".repeat(MAX_ELICITATION_PATTERN_INPUT_BYTES + 1);
        assert_eq!(
            validate_string_value("Value".into(), &schema, &oversized_value)
                .expect_err("oversized pattern input should be rejected")
                .to_string(),
            "Value is too long to validate safely"
        );
    }

    #[test]
    fn number_validation_rejects_non_finite_values() {
        let schema = acp::NumberPropertySchema::new();

        assert_eq!(
            validate_number_value("Amount".into(), &schema, "42.5")
                .expect("finite number should be accepted"),
            42.5
        );

        for value in ["NaN", "inf", "-inf", "1e309"] {
            assert_eq!(
                validate_number_value("Amount".into(), &schema, value)
                    .expect_err("non-finite number should be rejected")
                    .to_string(),
                "Amount must be a finite number"
            );
        }
    }

    #[test]
    fn should_render_pending_and_accepted_url_elicitations() {
        let pending = Elicitation {
            id: ElicitationEntryId("pending".into()),
            request: acp::CreateElicitationRequest::new(
                acp::ElicitationFormMode::new(
                    preview_request_scope(0),
                    acp::ElicitationSchema::new(),
                ),
                "Review this request.",
            ),
            status: pending_status(),
        };
        assert!(should_render_elicitation(&pending));

        let accepted_url = Elicitation {
            id: ElicitationEntryId("accepted-url".into()),
            request: acp::CreateElicitationRequest::new(
                acp::ElicitationUrlMode::new(
                    preview_request_scope(1),
                    acp::ElicitationId::new("accepted-url"),
                    "https://auth.example.com/device",
                ),
                "Authorize Zed in your browser.",
            ),
            status: ElicitationStatus::Accepted,
        };
        assert!(should_render_elicitation(&accepted_url));

        let accepted_form = Elicitation {
            id: ElicitationEntryId("accepted-form".into()),
            request: acp::CreateElicitationRequest::new(
                acp::ElicitationFormMode::new(
                    preview_request_scope(2),
                    acp::ElicitationSchema::new(),
                ),
                "Review this request.",
            ),
            status: ElicitationStatus::Accepted,
        };
        assert!(!should_render_elicitation(&accepted_form));

        let pending_unknown = Elicitation {
            id: ElicitationEntryId("pending-unknown".into()),
            request: acp::CreateElicitationRequest::new(
                acp::OtherElicitationMode::new("future", preview_request_scope(3), BTreeMap::new()),
                "Use a future input mode.",
            ),
            status: pending_status(),
        };
        assert!(!should_render_elicitation(&pending_unknown));
    }

    #[test]
    fn url_host_presentation_highlights_destination_and_suspicious_idn() {
        assert_eq!(
            url_host_presentation("https://auth.example.com/device"),
            Some(UrlHostPresentation {
                host: "auth.example.com".to_string(),
                suspicious_decoded_host: None,
            })
        );
        assert_eq!(
            url_host_presentation("https://xn--pple-43d.com/device"),
            Some(UrlHostPresentation {
                host: "xn--pple-43d.com".to_string(),
                suspicious_decoded_host: Some("\u{0430}pple.com".to_string()),
            })
        );
    }

    #[test]
    fn display_url_segments_never_elide_url_characters() {
        let url = format!(
            "https://auth.example.com/oauth/authorize?state={}",
            "a".repeat(MAX_URL_DISPLAY_SEGMENT_CHARS * 2)
        );

        let display_url_segments = display_url_segments(&url)
            .into_iter()
            .map(|segment| segment.to_string())
            .collect::<Vec<_>>();
        assert!(display_url_segments.len() > 1);
        assert!(
            !display_url_segments
                .iter()
                .any(|segment| segment.contains('…'))
        );
        assert!(
            display_url_segments
                .iter()
                .all(|segment| segment.chars().count() <= MAX_URL_DISPLAY_SEGMENT_CHARS)
        );
        assert_eq!(display_url_segments.concat(), url);
    }

    #[test]
    fn display_url_segments_use_larger_url_boundaries() {
        let url = "https://auth.example.com/oauth/authorize?client_id=zed-desktop&scope=repository";

        let display_url_segments = display_url_segments(url)
            .into_iter()
            .map(|segment| segment.to_string())
            .collect::<Vec<_>>();
        assert_eq!(display_url_segments.concat(), url);
        assert_eq!(display_url_segments[0], "https://auth.example.com/");
        assert!(
            display_url_segments
                .iter()
                .any(|segment| segment.ends_with('?'))
        );
        assert!(
            display_url_segments
                .iter()
                .any(|segment| segment.ends_with('&'))
        );
    }

    #[test]
    fn single_select_options_include_titled_descriptions() {
        let schema = acp::StringPropertySchema::new().one_of(vec![
            acp::EnumOption::new("production", "Production").description("Use live resources"),
        ]);

        let options = single_select_options(&schema);

        let [option] = options.as_slice() else {
            panic!("expected one option, got {}", options.len());
        };
        assert_eq!(option.value, "production");
        assert_eq!(option.label.to_string(), "Production");
        assert_eq!(
            option
                .description
                .as_ref()
                .map(|description| description.to_string()),
            Some("Use live resources".to_string())
        );
    }

    #[test]
    fn multi_select_options_include_titled_descriptions() {
        let schema = acp::MultiSelectPropertySchema::titled(vec![
            acp::EnumOption::new("repository", "Repository Access")
                .description("Read and update repositories"),
        ]);

        let options = multi_select_options(&schema);

        let [option] = options.as_slice() else {
            panic!("expected one option, got {}", options.len());
        };
        assert_eq!(option.value, "repository");
        assert_eq!(option.label.to_string(), "Repository Access");
        assert_eq!(
            option
                .description
                .as_ref()
                .map(|description| description.to_string()),
            Some("Read and update repositories".to_string())
        );
    }

    #[gpui::test]
    async fn form_state_preserves_string_whitespace(cx: &mut TestAppContext) {
        crate::conversation_view::tests::init_test(cx);

        let metadata = |level: &str| {
            serde_json::Map::from_iter([(
                "fixture".into(),
                serde_json::json!({"level": level, "nested": [null, {"enabled": true}]}),
            )])
        };
        let mut scopes = acp::MultiSelectPropertySchema::titled(vec![
            acp::EnumOption::new("repository", "Repository")
                .description("Read repositories")
                .meta(metadata("item-choice")),
        ])
        .default_value(vec!["repository".into()])
        .meta(metadata("array-property"));
        let acp::MultiSelectItems::Titled(items) = &mut scopes.items else {
            panic!("expected titled items");
        };
        items.meta = Some(metadata("items"));
        let schema = acp::ElicitationSchema::new()
            .meta(metadata("schema"))
            .property(
                "token",
                acp::StringPropertySchema::new()
                    .title("Token")
                    .default_value("  secret  ")
                    .format(acp::StringFormat::Other("future-token".into()))
                    .meta(metadata("string-property")),
                true,
            )
            .property(
                "environment",
                acp::StringPropertySchema::new()
                    .one_of(vec![
                        acp::EnumOption::new("production", "Production")
                            .description("Use live resources")
                            .meta(metadata("choice")),
                    ])
                    .meta(metadata("select-property")),
                true,
            )
            .property("scopes", scopes, true);
        let request = acp::CreateElicitationRequest::new(
            acp::ElicitationFormMode::new(preview_request_scope(0), schema),
            "Provide a token and access choices",
        )
        .meta(metadata("request"));
        let expected_content = BTreeMap::from([
            (
                "token".into(),
                acp::ElicitationContentValue::from("  secret  "),
            ),
            (
                "environment".into(),
                acp::ElicitationContentValue::from("production"),
            ),
            (
                "scopes".into(),
                acp::ElicitationContentValue::from(vec!["repository".to_string()]),
            ),
        ]);
        let store = cx.update(|cx| cx.new(|_| acp_thread::ElicitationStore::default()));
        let (elicitation_id, response_task) = store.update(cx, |store, cx| {
            store
                .request_elicitation_with_id(request.clone(), cx)
                .expect("form request should be accepted")
        });

        cx.add_window(|window, cx| {
            let schema = store.read_with(cx, |store, _cx| {
                let (_, elicitation) = store.elicitation(&elicitation_id).expect("stored form");
                assert_eq!(elicitation.request, request);
                let acp::ElicitationMode::Form(mode) = &elicitation.request.mode else {
                    panic!("expected form mode");
                };
                mode.requested_schema.clone()
            });
            let mut form_state = ElicitationFormState::new(&schema, window, cx);
            assert!(matches!(
                form_state.fields.get("token"),
                Some(ElicitationFieldState::Text(_))
            ));
            let submission = form_state
                .begin_submission(cx)
                .expect("submission should start");
            let content = submission
                .validate(&schema)
                .expect("unknown format annotations should not reject valid existing fields");
            assert!(form_state.validation_matches_current_values(&submission, cx));
            assert_eq!(content, expected_content);
            store.update(cx, |store, cx| {
                store.respond_to_elicitation(
                    &elicitation_id,
                    acp::CreateElicitationResponse::new(acp::ElicitationAction::Accept(
                        acp::ElicitationAcceptAction::new().content(content),
                    )),
                    cx,
                );
                let (_, elicitation) = store.elicitation(&elicitation_id).expect("submitted form");
                assert_eq!(elicitation.request, request);
                assert!(matches!(elicitation.status, ElicitationStatus::Accepted));
            });

            Editor::single_line(window, cx)
        });
        assert_eq!(
            response_task.await,
            acp::CreateElicitationResponse::new(acp::ElicitationAction::Accept(
                acp::ElicitationAcceptAction::new().content(expected_content),
            ))
        );
    }

    #[gpui::test]
    fn form_state_prevents_duplicate_submissions(cx: &mut TestAppContext) {
        crate::conversation_view::tests::init_test(cx);

        cx.add_window(|window, cx| {
            let schema = acp::ElicitationSchema::new().string("name", true);
            let mut form_state = ElicitationFormState::new(&schema, window, cx);

            assert!(form_state.begin_submission(cx).is_some());
            assert!(form_state.begin_submission(cx).is_none());

            form_state.set_errors(HashMap::default());
            assert!(form_state.begin_submission(cx).is_some());

            Editor::single_line(window, cx)
        });
    }

    #[gpui::test]
    fn form_state_discards_validation_for_stale_values(cx: &mut TestAppContext) {
        crate::conversation_view::tests::init_test(cx);

        cx.add_window(|window, cx| {
            let schema = acp::ElicitationSchema::new().property(
                "name",
                acp::StringPropertySchema::new().default_value("before"),
                true,
            );
            let mut form_state = ElicitationFormState::new(&schema, window, cx);
            let submission = form_state
                .begin_submission(cx)
                .expect("first submission should start");
            let editor = match form_state.fields.get("name") {
                Some(ElicitationFieldState::Text(editor)) => editor.clone(),
                _ => panic!("expected a text field"),
            };

            editor.update(cx, |editor, cx| editor.set_text("after", window, cx));

            assert!(!form_state.validation_matches_current_values(&submission, cx));
            assert!(form_state.begin_submission(cx).is_some());

            Editor::single_line(window, cx)
        });
    }

    #[gpui::test]
    fn form_state_discards_invalid_optional_single_select_default(cx: &mut TestAppContext) {
        crate::conversation_view::tests::init_test(cx);

        cx.add_window(|window, cx| {
            let schema = acp::ElicitationSchema::new().property(
                "environment",
                acp::StringPropertySchema::new()
                    .title("Environment")
                    .enum_values(vec!["production".to_string(), "staging".to_string()])
                    .default_value("development"),
                false,
            );
            let form_state = ElicitationFormState::new(&schema, window, cx);
            let content = form_state
                .collect(&schema, cx)
                .expect("invalid optional default should be ignored");

            assert_eq!(content.get("environment"), None);

            Editor::single_line(window, cx)
        });
    }

    #[gpui::test]
    fn form_state_replaces_invalid_required_single_select_default(cx: &mut TestAppContext) {
        crate::conversation_view::tests::init_test(cx);

        cx.add_window(|window, cx| {
            let schema = acp::ElicitationSchema::new().property(
                "environment",
                acp::StringPropertySchema::new()
                    .title("Environment")
                    .enum_values(vec!["production".to_string(), "staging".to_string()])
                    .default_value("development"),
                true,
            );
            let form_state = ElicitationFormState::new(&schema, window, cx);
            let content = form_state
                .collect(&schema, cx)
                .expect("required select should use the first valid choice");

            assert_eq!(
                content.get("environment"),
                Some(&acp::ElicitationContentValue::String(
                    "production".to_string()
                ))
            );

            Editor::single_line(window, cx)
        });
    }

    #[gpui::test]
    fn form_state_rejects_invalid_single_select_value(cx: &mut TestAppContext) {
        crate::conversation_view::tests::init_test(cx);

        cx.add_window(|window, cx| {
            let schema = acp::ElicitationSchema::new().property(
                "environment",
                acp::StringPropertySchema::new()
                    .title("Environment")
                    .enum_values(vec!["production".to_string(), "staging".to_string()]),
                false,
            );
            let mut form_state = ElicitationFormState::new(&schema, window, cx);
            form_state.set_single_select("environment", "development".to_string());

            let errors = form_state
                .collect(&schema, cx)
                .expect_err("invalid selected value should be rejected");
            assert_eq!(
                errors
                    .get("environment")
                    .expect("environment should have an error")
                    .to_string(),
                "Environment must be one of the provided options"
            );

            Editor::single_line(window, cx)
        });
    }

    #[gpui::test]
    fn form_state_reports_all_validation_errors(cx: &mut TestAppContext) {
        crate::conversation_view::tests::init_test(cx);

        cx.add_window(|window, cx| {
            let schema = acp::ElicitationSchema::new()
                .string("account", true)
                .property(
                    "age",
                    acp::IntegerPropertySchema::new().title("Age").minimum(18),
                    true,
                )
                .property(
                    "environment",
                    acp::StringPropertySchema::new()
                        .title("Environment")
                        .enum_values(vec!["production".to_string(), "staging".to_string()]),
                    false,
                );
            let mut form_state = ElicitationFormState::new(&schema, window, cx);
            if let Some(ElicitationFieldState::Text(editor)) = form_state.fields.get("age") {
                editor.update(cx, |editor, cx| editor.set_text("abc", window, cx));
            }
            form_state.set_single_select("environment", "development".to_string());

            let errors = form_state
                .collect(&schema, cx)
                .expect_err("all invalid fields should be reported");
            assert_eq!(
                errors
                    .get("account")
                    .expect("account should have an error")
                    .to_string(),
                "account is required"
            );
            assert_eq!(
                errors
                    .get("age")
                    .expect("age should have an error")
                    .to_string(),
                "Age must be an integer"
            );
            assert_eq!(
                errors
                    .get("environment")
                    .expect("environment should have an error")
                    .to_string(),
                "Environment must be one of the provided options"
            );

            Editor::single_line(window, cx)
        });
    }
}

#[derive(RegisterComponent)]
pub struct ElicitationCardPreview;

impl Component for ElicitationCardPreview {
    fn scope() -> ComponentScope {
        ComponentScope::Agent
    }

    fn description() -> &'static str {
        "ACP elicitation request cards as rendered in the agent panel."
    }

    fn preview(window: &mut Window, cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .children([
                example_group_with_title(
                    "Form Requests",
                    vec![
                        single_example(
                            "Pending Form",
                            render_form_preview(0, pending_status(), &[], window, cx),
                        )
                        .width(px(640.)),
                        single_example(
                            "Validation Errors",
                            render_form_preview(
                                1,
                                pending_status(),
                                &[
                                    ("account_name", "Account name is required"),
                                    ("environment", "Choose an environment"),
                                    ("scopes", "Choose at least one access scope"),
                                ],
                                window,
                                cx,
                            ),
                        )
                        .width(px(640.)),
                    ],
                )
                .vertical()
                .into_any_element(),
                example_group_with_title(
                    "URL Requests",
                    vec![
                        single_example(
                            "URL Consent",
                            render_url_preview(3, pending_status(), window, cx),
                        )
                        .width(px(640.)),
                    ],
                )
                .vertical()
                .into_any_element(),
                example_group_with_title(
                    "Terminal States",
                    vec![
                        single_example(
                            "Declined",
                            render_form_preview(6, ElicitationStatus::Declined, &[], window, cx),
                        )
                        .width(px(640.)),
                        single_example(
                            "Canceled",
                            render_form_preview(7, ElicitationStatus::Canceled, &[], window, cx),
                        )
                        .width(px(640.)),
                    ],
                )
                .vertical()
                .into_any_element(),
            ])
            .into_any_element()
    }
}

fn render_form_preview(
    entry_ix: usize,
    status: ElicitationStatus,
    field_errors: &[(&'static str, &'static str)],
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let request = acp::CreateElicitationRequest::new(
        acp::ElicitationFormMode::new(preview_request_scope(entry_ix), preview_form_schema()),
        "Choose how Zed should connect to this account.",
    );
    let mut form_state = matches!(status, ElicitationStatus::Pending { .. }).then(|| {
        let acp::ElicitationMode::Form(mode) = &request.mode else {
            unreachable!();
        };
        ElicitationFormState::new(&mode.requested_schema, window, cx)
    });
    if let Some(form_state) = &mut form_state {
        for (field_name, error) in field_errors {
            form_state.set_field_error(*field_name, *error);
        }
    }

    render_preview_card(entry_ix, request, status, form_state.as_ref(), cx)
}

fn preview_url() -> &'static str {
    "https://auth.example.com/oauth/authorize?client_id=zed-desktop&redirect_uri=zed%3A%2F%2Fagent%2Facp%2Fcallback&scope=profile%20repository%20terminal&state=9b8b0a873a1e4b57b7f9f7b6d2d3d0f4"
}

fn render_url_preview(
    entry_ix: usize,
    status: ElicitationStatus,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let request = acp::CreateElicitationRequest::new(
        acp::ElicitationUrlMode::new(
            preview_request_scope(entry_ix),
            acp::ElicitationId::new(format!("preview-url-{entry_ix}")),
            preview_url(),
        ),
        "Authorize Zed in your browser to finish signing in.",
    );

    render_preview_card(entry_ix, request, status, None, cx)
}

fn render_preview_card(
    entry_ix: usize,
    request: acp::CreateElicitationRequest,
    status: ElicitationStatus,
    form_state: Option<&ElicitationFormState>,
    cx: &App,
) -> AnyElement {
    let elicitation = Elicitation {
        id: ElicitationEntryId(format!("preview-elicitation-{entry_ix}").into()),
        request,
        status,
    };

    div()
        .w_full()
        .max_w(px(640.))
        .child(
            ElicitationCard::new(
                entry_ix,
                &elicitation,
                "Example Agent".into(),
                form_state,
                ElicitationCardHandlers::noop(),
            )
            .render(cx),
        )
        .into_any_element()
}

fn pending_status() -> ElicitationStatus {
    let (respond_tx, _response_rx) = oneshot::channel();
    ElicitationStatus::Pending { respond_tx }
}

fn preview_request_scope(index: usize) -> acp::ElicitationRequestScope {
    acp::ElicitationRequestScope::new(acp::RequestId::Number(index as i64))
}

fn preview_form_schema() -> acp::ElicitationSchema {
    acp::ElicitationSchema::new()
        .property(
            "account_name",
            acp::StringPropertySchema::new()
                .title("Account Name")
                .description("Used to label this connection in the agent panel.")
                .default_value("Work"),
            true,
        )
        .property(
            "environment",
            acp::StringPropertySchema::new()
                .title("Environment")
                .description("Select the environment this credential should target.")
                .one_of(vec![
                    acp::EnumOption::new("production", "Production")
                        .description("Use the live account and production resources."),
                    acp::EnumOption::new("staging", "Staging")
                        .description("Validate changes against staging data first."),
                    acp::EnumOption::new("development", "Development"),
                ])
                .default_value("staging"),
            true,
        )
        .property(
            "scopes",
            acp::MultiSelectPropertySchema::titled(vec![
                acp::EnumOption::new("profile", "Profile")
                    .description("Read account identity and basic profile details."),
                acp::EnumOption::new("repository", "Repository Access")
                    .description("Read and update repositories connected to this account."),
                acp::EnumOption::new("terminal", "Terminal Commands"),
            ])
            .title("Access")
            .description("Choose what the agent can use for this authorization.")
            .min_items(1)
            .default_value(vec!["profile".to_string(), "repository".to_string()]),
            true,
        )
        .property(
            "remember",
            acp::BooleanPropertySchema::new()
                .title("Remember Authorization")
                .description("Store this authorization for future sessions.")
                .default_value(true),
            false,
        )
}

fn single_select_options(schema: &acp::StringPropertySchema) -> Vec<ElicitationOption> {
    if let Some(options) = &schema.one_of {
        return options
            .iter()
            .map(|option| ElicitationOption {
                value: option.value.clone(),
                label: SharedString::from(option.title.clone()),
                description: option.description.clone().map(SharedString::from),
            })
            .collect();
    }

    schema
        .enum_values
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|value| ElicitationOption {
            value: value.clone(),
            label: SharedString::from(value.clone()),
            description: None,
        })
        .collect()
}

fn single_select_default_value(
    schema: &acp::StringPropertySchema,
    options: &[ElicitationOption],
) -> Option<String> {
    schema
        .default
        .as_ref()
        .filter(|default| {
            options
                .iter()
                .any(|option| option.value.as_str() == default.as_str())
        })
        .cloned()
}

fn multi_select_options(schema: &acp::MultiSelectPropertySchema) -> Vec<ElicitationOption> {
    match &schema.items {
        acp::MultiSelectItems::String(items) => items
            .values
            .iter()
            .map(|value| ElicitationOption {
                value: value.clone(),
                label: SharedString::from(value.clone()),
                description: None,
            })
            .collect(),
        acp::MultiSelectItems::Titled(items) => items
            .options
            .iter()
            .map(|option| ElicitationOption {
                value: option.value.clone(),
                label: SharedString::from(option.title.clone()),
                description: option.description.clone().map(SharedString::from),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn validate_number_value(
    title: SharedString,
    schema: &acp::NumberPropertySchema,
    value: &str,
) -> Result<f64, SharedString> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| SharedString::from(format!("{title} must be a number")))?;
    if !parsed.is_finite() {
        return Err(format!("{title} must be a finite number").into());
    }
    if let Some(minimum) = schema.minimum
        && parsed < minimum
    {
        return Err(format!("{title} must be at least {minimum}").into());
    }
    if let Some(maximum) = schema.maximum
        && parsed > maximum
    {
        return Err(format!("{title} must be at most {maximum}").into());
    }

    Ok(parsed)
}

fn validate_integer_value(
    title: SharedString,
    schema: &acp::IntegerPropertySchema,
    value: &str,
) -> Result<i64, SharedString> {
    let parsed = value
        .parse::<i64>()
        .map_err(|_| SharedString::from(format!("{title} must be an integer")))?;
    if let Some(minimum) = schema.minimum
        && parsed < minimum
    {
        return Err(format!("{title} must be at least {minimum}").into());
    }
    if let Some(maximum) = schema.maximum
        && parsed > maximum
    {
        return Err(format!("{title} must be at most {maximum}").into());
    }

    Ok(parsed)
}

fn validate_string_value(
    title: SharedString,
    schema: &acp::StringPropertySchema,
    value: &str,
) -> Result<(), SharedString> {
    let length = value.chars().count();
    if schema
        .min_length
        .is_some_and(|min_length| length < min_length as usize)
    {
        return Err(format!("{title} is too short").into());
    }
    if schema
        .max_length
        .is_some_and(|max_length| length > max_length as usize)
    {
        return Err(format!("{title} is too long").into());
    }

    validate_string_pattern_and_format(title, schema, value)
}

fn validate_single_select_value(
    title: SharedString,
    schema: &acp::StringPropertySchema,
    value: &str,
) -> Result<(), SharedString> {
    let options = single_select_options(schema);
    if options.iter().any(|option| option.value.as_str() == value) {
        Ok(())
    } else {
        Err(format!("{title} must be one of the provided options").into())
    }
}

const MAX_ELICITATION_PATTERN_BYTES: usize = 16 * 1024;
const MAX_ELICITATION_PATTERN_INPUT_BYTES: usize = 1024 * 1024;
const ELICITATION_PATTERN_BACKTRACK_LIMIT: usize = 10_000;
const ELICITATION_PATTERN_COMPILED_SIZE_LIMIT: usize = 1024 * 1024;
const ELICITATION_PATTERN_DFA_SIZE_LIMIT: usize = 1024 * 1024;

fn validate_string_pattern_and_format(
    title: SharedString,
    schema: &acp::StringPropertySchema,
    value: &str,
) -> Result<(), SharedString> {
    if schema.pattern.is_none()
        && schema
            .format
            .as_ref()
            .and_then(string_format_json_name)
            .is_none()
    {
        return Ok(());
    }
    if schema
        .pattern
        .as_ref()
        .is_some_and(|pattern| pattern.len() > MAX_ELICITATION_PATTERN_BYTES)
    {
        return Err(format!("{title} has an invalid validation pattern").into());
    }
    if schema.pattern.is_some() && value.len() > MAX_ELICITATION_PATTERN_INPUT_BYTES {
        return Err(format!("{title} is too long to validate safely").into());
    }

    let mut validation_schema = serde_json::Map::new();
    validation_schema.insert(
        "type".to_string(),
        serde_json::Value::String("string".into()),
    );
    if let Some(pattern) = &schema.pattern {
        validation_schema.insert(
            "pattern".to_string(),
            serde_json::Value::String(pattern.clone()),
        );
    }
    if let Some(format) = schema.format.as_ref().and_then(string_format_json_name) {
        validation_schema.insert(
            "format".to_string(),
            serde_json::Value::String(format.into()),
        );
    }

    let validation_schema = serde_json::Value::Object(validation_schema);
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .with_pattern_options(
            jsonschema::PatternOptions::fancy_regex()
                .backtrack_limit(ELICITATION_PATTERN_BACKTRACK_LIMIT)
                .size_limit(ELICITATION_PATTERN_COMPILED_SIZE_LIMIT)
                .dfa_size_limit(ELICITATION_PATTERN_DFA_SIZE_LIMIT),
        )
        .build(&validation_schema)
        .map_err(|_| {
            if schema.pattern.is_some() {
                format!("{title} has an invalid validation pattern")
            } else {
                format!("{title} has an invalid validation format")
            }
        })?;
    let value = serde_json::Value::String(value.to_string());
    if let Err(error) = validator.validate(&value) {
        if matches!(
            error.kind(),
            jsonschema::error::ValidationErrorKind::BacktrackLimitExceeded { .. }
        ) {
            return Err(format!("{title} has a validation pattern that is too complex").into());
        }
    } else {
        return Ok(());
    }

    match (
        schema.pattern.is_some(),
        schema.format.as_ref().and_then(string_format_label),
    ) {
        (true, Some(_)) => Err(format!("{title} does not match the requested constraints").into()),
        (true, None) => Err(format!("{title} does not match the requested pattern").into()),
        (false, Some(format)) => Err(format!("{title} must be {format}").into()),
        (false, None) => Ok(()),
    }
}

fn string_format_json_name(format: &acp::StringFormat) -> Option<&'static str> {
    match format {
        acp::StringFormat::Email => Some("email"),
        acp::StringFormat::Uri => Some("uri"),
        acp::StringFormat::Date => Some("date"),
        acp::StringFormat::DateTime => Some("date-time"),
        _ => None,
    }
}

fn string_format_label(format: &acp::StringFormat) -> Option<&'static str> {
    match format {
        acp::StringFormat::Email => Some("an email address"),
        acp::StringFormat::Uri => Some("a URI"),
        acp::StringFormat::Date => Some("a date"),
        acp::StringFormat::DateTime => Some("a date and time"),
        _ => None,
    }
}

fn property_title(name: &str, property: &acp::ElicitationPropertySchema) -> SharedString {
    let title = match property {
        acp::ElicitationPropertySchema::String(schema) => schema.title.as_deref(),
        acp::ElicitationPropertySchema::Number(schema) => schema.title.as_deref(),
        acp::ElicitationPropertySchema::Integer(schema) => schema.title.as_deref(),
        acp::ElicitationPropertySchema::Boolean(schema) => schema.title.as_deref(),
        acp::ElicitationPropertySchema::Array(schema) => schema.title.as_deref(),
        _ => None,
    };
    SharedString::from(title.unwrap_or(name).to_string())
}

fn property_description(property: &acp::ElicitationPropertySchema) -> Option<SharedString> {
    match property {
        acp::ElicitationPropertySchema::String(schema) => schema.description.clone(),
        acp::ElicitationPropertySchema::Number(schema) => schema.description.clone(),
        acp::ElicitationPropertySchema::Integer(schema) => schema.description.clone(),
        acp::ElicitationPropertySchema::Boolean(schema) => schema.description.clone(),
        acp::ElicitationPropertySchema::Array(schema) => schema.description.clone(),
        _ => None,
    }
    .map(SharedString::from)
}

type RespondHandler = Rc<dyn Fn(ElicitationEntryId, &mut Window, &mut App)>;
type OpenUrlHandler = Rc<dyn Fn(ElicitationEntryId, String, &mut Window, &mut App)>;
type BooleanHandler = Rc<dyn Fn(ElicitationEntryId, String, bool, &mut App)>;
type SelectHandler = Rc<dyn Fn(ElicitationEntryId, String, String, &mut App)>;
type MultiSelectHandler = Rc<dyn Fn(ElicitationEntryId, String, String, bool, &mut App)>;

#[derive(Clone)]
pub(crate) struct ElicitationCardHandlers {
    on_submit: RespondHandler,
    on_decline: RespondHandler,
    on_cancel: RespondHandler,
    on_dismiss_url: RespondHandler,
    on_open_url: OpenUrlHandler,
    on_boolean_change: BooleanHandler,
    on_single_select_change: SelectHandler,
    on_multi_select_change: MultiSelectHandler,
}

impl ElicitationCardHandlers {
    pub(crate) fn new(
        on_submit: impl Fn(ElicitationEntryId, &mut Window, &mut App) + 'static,
        on_decline: impl Fn(ElicitationEntryId, &mut Window, &mut App) + 'static,
        on_cancel: impl Fn(ElicitationEntryId, &mut Window, &mut App) + 'static,
        on_dismiss_url: impl Fn(ElicitationEntryId, &mut Window, &mut App) + 'static,
        on_open_url: impl Fn(ElicitationEntryId, String, &mut Window, &mut App) + 'static,
        on_boolean_change: impl Fn(ElicitationEntryId, String, bool, &mut App) + 'static,
        on_single_select_change: impl Fn(ElicitationEntryId, String, String, &mut App) + 'static,
        on_multi_select_change: impl Fn(ElicitationEntryId, String, String, bool, &mut App) + 'static,
    ) -> Self {
        Self {
            on_submit: Rc::new(on_submit),
            on_decline: Rc::new(on_decline),
            on_cancel: Rc::new(on_cancel),
            on_dismiss_url: Rc::new(on_dismiss_url),
            on_open_url: Rc::new(on_open_url),
            on_boolean_change: Rc::new(on_boolean_change),
            on_single_select_change: Rc::new(on_single_select_change),
            on_multi_select_change: Rc::new(on_multi_select_change),
        }
    }

    /// Runs `followup` after each single-select choice, such as moving on to
    /// the next question.
    pub(crate) fn after_single_select(mut self, followup: impl Fn(&mut App) + 'static) -> Self {
        let on_single_select_change = self.on_single_select_change.clone();
        self.on_single_select_change = Rc::new(move |elicitation_id, field_name, value, cx| {
            on_single_select_change(elicitation_id, field_name, value, cx);
            followup(cx);
        });
        self
    }

    pub(crate) fn noop() -> Self {
        Self::new(
            |_, _, _| {},
            |_, _, _| {},
            |_, _, _| {},
            |_, _, _| {},
            |_, _, _, _| {},
            |_, _, _, _| {},
            |_, _, _, _| {},
            |_, _, _, _, _| {},
        )
    }
}

pub(crate) fn should_render_elicitation(elicitation: &Elicitation) -> bool {
    matches!(
        (&elicitation.status, &elicitation.request.mode),
        (
            ElicitationStatus::Pending { .. },
            acp::ElicitationMode::Form(_) | acp::ElicitationMode::Url(_)
        ) | (ElicitationStatus::Accepted, acp::ElicitationMode::Url(_))
    )
}

const MIN_URL_DISPLAY_SEGMENT_CHARS: usize = 16;
const MAX_URL_DISPLAY_SEGMENT_CHARS: usize = 64;

#[derive(Debug, PartialEq, Eq)]
struct UrlHostPresentation {
    host: String,
    suspicious_decoded_host: Option<String>,
}

fn url_host_presentation(url: &str) -> Option<UrlHostPresentation> {
    let url = url::Url::parse(url).ok()?;
    let host = url.host_str()?.to_string();
    let (decoded_host, suspicious_characters) = crate::unicode_confusables::scan_host(&host);

    Some(UrlHostPresentation {
        host,
        suspicious_decoded_host: (!suspicious_characters.is_empty()).then_some(decoded_host),
    })
}

fn display_url_segments(url: &str) -> Vec<SharedString> {
    let mut segments = Vec::new();
    let mut segment = String::new();
    let mut segment_chars = 0;
    let mut characters = url.chars().peekable();

    while let Some(character) = characters.next() {
        segment.push(character);
        segment_chars += 1;

        let should_split_at_boundary = segment_chars >= MIN_URL_DISPLAY_SEGMENT_CHARS
            && is_url_display_segment_boundary(character);
        let should_split_at_length = segment_chars >= MAX_URL_DISPLAY_SEGMENT_CHARS;

        if characters.peek().is_some() && (should_split_at_boundary || should_split_at_length) {
            segments.push(std::mem::take(&mut segment).into());
            segment_chars = 0;
        }
    }

    if !segment.is_empty() {
        segments.push(segment.into());
    }

    segments
}

fn is_url_display_segment_boundary(character: char) -> bool {
    matches!(character, '/' | '?' | '&' | '#')
}

pub(crate) struct ElicitationCard<'a> {
    entry_ix: usize,
    elicitation: &'a Elicitation,
    requester_name: SharedString,
    form_state: Option<&'a ElicitationFormState>,
    handlers: ElicitationCardHandlers,
}

impl<'a> ElicitationCard<'a> {
    pub(crate) fn new(
        entry_ix: usize,
        elicitation: &'a Elicitation,
        requester_name: SharedString,
        form_state: Option<&'a ElicitationFormState>,
        handlers: ElicitationCardHandlers,
    ) -> Self {
        Self {
            entry_ix,
            elicitation,
            requester_name,
            form_state,
            handlers,
        }
    }

    pub(crate) fn render(self, cx: &App) -> Div {
        let border_color = cx.theme().colors().border.opacity(0.8);
        let header_background = cx
            .theme()
            .colors()
            .element_background
            .blend(cx.theme().colors().editor_foreground.opacity(0.025));
        let tool_name_font_size = rems_from_px(13_f32);
        let is_pending = matches!(&self.elicitation.status, ElicitationStatus::Pending { .. });
        let is_accepted_url = matches!(
            (&self.elicitation.status, &self.elicitation.request.mode),
            (ElicitationStatus::Accepted, acp::ElicitationMode::Url(_))
        );
        let (status_label, status_icon, status_color) = match &self.elicitation.status {
            ElicitationStatus::Pending { .. } => ("Waiting for input", IconName::Info, Color::Info),
            ElicitationStatus::Accepted if is_accepted_url => {
                ("Waiting for completion", IconName::Info, Color::Info)
            }
            ElicitationStatus::Accepted => ("Submitted", IconName::Check, Color::Success),
            ElicitationStatus::Declined => ("Declined", IconName::Close, Color::Muted),
            ElicitationStatus::Canceled => ("Canceled", IconName::Circle, Color::Muted),
            ElicitationStatus::Completed => ("Completed", IconName::Check, Color::Success),
        };

        let body = v_flex()
            .gap_2()
            .p_3()
            .child(Label::new(self.elicitation.request.message.clone()).size(LabelSize::Small));
        let body = match &self.elicitation.request.mode {
            acp::ElicitationMode::Form(mode) if is_pending => {
                body.child(self.render_form(mode, cx))
            }
            acp::ElicitationMode::Url(mode) if is_pending || is_accepted_url => {
                body.child(self.render_url_elicitation(mode))
            }
            _ => body,
        };

        v_flex()
            .key_context("Elicitation")
            .tab_group()
            .on_action(|_: &menu::SelectNext, window, cx| window.focus_next(cx))
            .on_action(|_: &menu::SelectPrevious, window, cx| window.focus_prev(cx))
            .mx_5()
            .my_1p5()
            .rounded_md()
            .border_1()
            .border_color(border_color)
            .overflow_hidden()
            .child(
                h_flex()
                    .h_8()
                    .p_1()
                    .w_full()
                    .justify_between()
                    .bg(header_background)
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1p5()
                            .px_1()
                            .child(
                                Icon::new(status_icon)
                                    .size(IconSize::Small)
                                    .color(status_color),
                            )
                            .child(
                                Label::new(format!("Input Requested by {}", self.requester_name))
                                    .size(LabelSize::Custom(tool_name_font_size))
                                    .truncate(),
                            ),
                    )
                    .child(
                        Label::new(status_label)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(body)
            .when(is_pending || is_accepted_url, |this| {
                this.child(self.render_actions(cx))
            })
    }

    fn render_form(&self, mode: &acp::ElicitationFormMode, cx: &App) -> AnyElement {
        let Some(state) = self.form_state else {
            return Empty.into_any_element();
        };

        v_flex()
            .gap_2()
            .when_some(mode.requested_schema.title.clone(), |this, title| {
                this.child(Label::new(title).size(LabelSize::Small))
            })
            .when_some(
                mode.requested_schema.description.clone(),
                |this, description| {
                    this.child(
                        Label::new(description)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                },
            )
            .children(mode.requested_schema.properties.iter().filter_map(
                |(field_name, property)| {
                    let field = state.fields.get(field_name)?;
                    Some(self.render_field(
                        field_name,
                        property,
                        field,
                        state.field_errors.get(field_name),
                        cx,
                    ))
                },
            ))
            .into_any_element()
    }

    fn render_fields(&self, field_names: &[String], cx: &App) -> AnyElement {
        let acp::ElicitationMode::Form(mode) = &self.elicitation.request.mode else {
            return Empty.into_any_element();
        };
        let Some(state) = self.form_state else {
            return Empty.into_any_element();
        };
        v_flex()
            .gap_2()
            .children(field_names.iter().filter_map(|field_name| {
                let property = mode.requested_schema.properties.get(field_name)?;
                let field = state.fields.get(field_name)?;
                Some(self.render_field(
                    field_name,
                    property,
                    field,
                    state.field_errors.get(field_name),
                    cx,
                ))
            }))
            .into_any_element()
    }

    fn render_field(
        &self,
        field_name: &str,
        property: &acp::ElicitationPropertySchema,
        field: &ElicitationFieldState,
        error: Option<&SharedString>,
        cx: &App,
    ) -> AnyElement {
        let label = property_title(field_name, property);
        let description = property_description(property);
        let border_color = cx.theme().colors().border.opacity(0.8);
        let focused_border_color = cx.theme().colors().border_focused;
        let field_border_color = if error.is_some() {
            Color::Error.color(cx)
        } else {
            border_color
        };
        let editor_background = cx.theme().colors().editor_background;
        let label_color = if error.is_some() {
            Color::Error
        } else {
            Color::Default
        };

        if let ElicitationFieldState::Boolean(value) = field {
            let checkbox_state = if *value {
                ToggleState::Selected
            } else {
                ToggleState::Unselected
            };
            let next_value = !*value;
            let on_boolean_change = self.handlers.on_boolean_change.clone();
            let elicitation_id = self.elicitation.id.clone();
            let field_name = field_name.to_string();
            let row_id = format!("elicitation-bool-row-{}-{field_name}", self.entry_ix);
            let checkbox_id = format!("elicitation-bool-{}-{field_name}", self.entry_ix);

            return v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .id(row_id)
                        .tab_index(0)
                        .w_full()
                        .items_start()
                        .gap_1()
                        .cursor_pointer()
                        .focus_visible(|this| this.bg(cx.theme().colors().element_hover))
                        .on_click(move |_, _window, cx| {
                            on_boolean_change(
                                elicitation_id.clone(),
                                field_name.clone(),
                                next_value,
                                cx,
                            );
                        })
                        .child(div().child(Checkbox::new(checkbox_id, checkbox_state)))
                        .child(
                            v_flex()
                                .gap_0p5()
                                .child(Label::new(label).size(LabelSize::Small).color(label_color))
                                .when_some(description, |this, description| {
                                    this.child(
                                        Label::new(description)
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                }),
                        ),
                )
                .when_some(error.cloned(), |this, error| {
                    this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                })
                .into_any_element();
        }

        let group_label = label.clone();
        let label = if error.is_some() {
            Label::new(label).size(LabelSize::Small).color(Color::Error)
        } else {
            Label::new(label).size(LabelSize::Small)
        };

        v_flex()
            .gap_1()
            .child(label)
            .when_some(description, |this, description| {
                this.child(
                    Label::new(description)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .child(match field {
                ElicitationFieldState::Text(editor) => {
                    let on_submit = self.handlers.on_submit.clone();
                    let elicitation_id = self.elicitation.id.clone();
                    let is_submitting = self.form_state.is_some_and(|state| state.is_submitting);

                    div()
                        .track_focus(&editor.focus_handle(cx).tab_stop(true))
                        .on_action(move |_: &menu::Confirm, window, cx| {
                            if !is_submitting {
                                on_submit(elicitation_id.clone(), window, cx);
                            }
                        })
                        .rounded_sm()
                        .border_1()
                        .border_color(field_border_color)
                        .focus_visible(|this| this.border_color(focused_border_color))
                        .bg(editor_background)
                        .px_1()
                        .py_0p5()
                        .text_xs()
                        .child(editor.clone().into_any_element())
                        .into_any_element()
                }
                ElicitationFieldState::Boolean(_) => Empty.into_any_element(),
                ElicitationFieldState::SingleSelect { value } => {
                    let options = match property {
                        acp::ElicitationPropertySchema::String(schema) => {
                            single_select_options(schema)
                        }
                        _ => Vec::new(),
                    };
                    self.render_single_select(
                        field_name,
                        group_label,
                        value.as_ref(),
                        options,
                        error.is_some(),
                    )
                }
                ElicitationFieldState::MultiSelect(selected) => {
                    let options = match property {
                        acp::ElicitationPropertySchema::Array(schema) => {
                            multi_select_options(schema)
                        }
                        _ => Vec::new(),
                    };
                    v_flex()
                        .id(format!("elicitation-multi-{}-{field_name}", self.entry_ix))
                        .role(Role::Group)
                        .aria_label(group_label)
                        .gap_1()
                        .children(options.into_iter().map(|option| {
                            let is_selected = selected.contains(&option.value);
                            let on_multi_select_change =
                                self.handlers.on_multi_select_change.clone();
                            let elicitation_id = self.elicitation.id.clone();
                            let field_name = field_name.to_string();
                            let card_id = format!(
                                "elicitation-multi-option-{}-{field_name}-{}",
                                self.entry_ix, option.value
                            );
                            let value = option.value;
                            ChoiceCard::checkbox(card_id, option.label, is_selected)
                                .when_some(option.description, |this, description| {
                                    this.description(description)
                                })
                                .invalid(error.is_some())
                                .on_click(move |_, _window, cx| {
                                    on_multi_select_change(
                                        elicitation_id.clone(),
                                        field_name.clone(),
                                        value.clone(),
                                        !is_selected,
                                        cx,
                                    );
                                })
                        }))
                        .into_any_element()
                }
            })
            .when_some(error.cloned(), |this, error| {
                this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
            })
            .into_any_element()
    }

    fn render_single_select(
        &self,
        field_name: &str,
        group_label: SharedString,
        selected_value: Option<&String>,
        options: Vec<ElicitationOption>,
        has_error: bool,
    ) -> AnyElement {
        let entry_ix = self.entry_ix;
        let elicitation_id = self.elicitation.id.clone();
        let field_name = field_name.to_string();
        let on_single_select_change = self.handlers.on_single_select_change.clone();

        v_flex()
            .id(format!("elicitation-select-{entry_ix}-{field_name}"))
            .role(Role::RadioGroup)
            .aria_label(group_label)
            .gap_1()
            .children(options.into_iter().map(|option| {
                let card_id = format!(
                    "elicitation-select-option-{entry_ix}-{field_name}-{}",
                    option.value
                );
                let is_selected =
                    selected_value.is_some_and(|selected_value| selected_value == &option.value);
                let option_value = option.value;
                let elicitation_id = elicitation_id.clone();
                let field_name = field_name.clone();
                let on_single_select_change = on_single_select_change.clone();

                ChoiceCard::radio(card_id, option.label, is_selected)
                    .when_some(option.description, |this, description| {
                        this.description(description)
                    })
                    .invalid(has_error)
                    .on_click(move |_, _window, cx| {
                        on_single_select_change(
                            elicitation_id.clone(),
                            field_name.clone(),
                            option_value.clone(),
                            cx,
                        );
                    })
            }))
            .into_any_element()
    }

    fn render_url_elicitation(&self, mode: &acp::ElicitationUrlMode) -> AnyElement {
        v_flex()
            .gap_2()
            .when_some(url_host_presentation(&mode.url), |this, presentation| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Label::new("Destination")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(Label::new(presentation.host).size(LabelSize::Small)),
                        )
                        .when_some(
                            presentation.suspicious_decoded_host,
                            |this, decoded_host| {
                                this.child(
                                    h_flex()
                                        .items_start()
                                        .gap_1()
                                        .child(
                                            Icon::new(IconName::Warning)
                                                .size(IconSize::XSmall)
                                                .color(Color::Warning),
                                        )
                                        .child(
                                            Label::new(format!(
                                                "This internationalized address displays as {decoded_host}. Verify it carefully."
                                            ))
                                            .size(LabelSize::Small)
                                            .color(Color::Warning),
                                        ),
                                )
                            },
                        ),
                )
            })
            .child(Self::render_url_summary(&mode.url))
            .into_any_element()
    }

    fn render_url_summary(url: &str) -> AnyElement {
        h_flex()
            .gap_1()
            .w_full()
            .min_w_0()
            .items_start()
            .child(
                div().h(rems_from_px(16_f32)).flex().items_center().child(
                    Icon::new(IconName::Link)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                ),
            )
            .child(h_flex().min_w_0().flex_1().flex_wrap().children(
                display_url_segments(url).into_iter().map(|segment| {
                    Label::new(segment)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                }),
            ))
            .into_any_element()
    }

    fn render_actions(&self, cx: &App) -> AnyElement {
        let open_url = match &self.elicitation.request.mode {
            acp::ElicitationMode::Url(mode) => Some(mode.url.clone()),
            _ => None,
        };
        let is_accepted_url =
            open_url.is_some() && matches!(self.elicitation.status, ElicitationStatus::Accepted);
        let is_submitting = self.form_state.is_some_and(|state| state.is_submitting);
        let (accept_label, accept_icon, accept_icon_color) = if is_accepted_url {
            ("Open Again", IconName::ArrowUpRight, Color::Muted)
        } else if open_url.is_some() {
            ("Open", IconName::ArrowUpRight, Color::Muted)
        } else {
            ("Submit", IconName::Check, Color::Success)
        };
        let border_color = cx.theme().colors().border.opacity(0.8);
        let on_submit = self.handlers.on_submit.clone();
        let on_open_url = self.handlers.on_open_url.clone();
        let on_decline = self.handlers.on_decline.clone();
        let on_cancel = self.handlers.on_cancel.clone();
        let on_dismiss_url = self.handlers.on_dismiss_url.clone();
        let submit_id = self.elicitation.id.clone();
        let decline_id = self.elicitation.id.clone();
        let cancel_id = self.elicitation.id.clone();
        let dismiss_id = self.elicitation.id.clone();

        h_flex()
            .w_full()
            .p_1()
            .gap_1()
            .justify_end()
            .border_t_1()
            .border_color(border_color)
            .child(
                Button::new(("elicitation-accept", self.entry_ix), accept_label)
                    .tab_index(0_isize)
                    .start_icon(
                        Icon::new(accept_icon)
                            .size(IconSize::XSmall)
                            .color(accept_icon_color),
                    )
                    .label_size(LabelSize::Small)
                    .disabled(is_submitting)
                    .on_click(move |_, window, cx| {
                        if let Some(url) = &open_url {
                            on_open_url(submit_id.clone(), url.clone(), window, cx);
                            if !is_accepted_url {
                                on_submit(submit_id.clone(), window, cx);
                            }
                        } else {
                            on_submit(submit_id.clone(), window, cx);
                        }
                    }),
            )
            .when(!is_accepted_url, |this| {
                this.child(
                    Button::new(("elicitation-decline", self.entry_ix), "Decline")
                        .tab_index(0_isize)
                        .start_icon(
                            Icon::new(IconName::Close)
                                .size(IconSize::XSmall)
                                .color(Color::Error),
                        )
                        .label_size(LabelSize::Small)
                        .on_click(move |_, window, cx| {
                            on_decline(decline_id.clone(), window, cx);
                        }),
                )
                .child(
                    Button::new(("elicitation-cancel", self.entry_ix), "Cancel")
                        .tab_index(0_isize)
                        .label_size(LabelSize::Small)
                        .on_click(move |_, window, cx| {
                            on_cancel(cancel_id.clone(), window, cx);
                        }),
                )
            })
            .when(is_accepted_url, |this| {
                this.child(
                    Button::new(("elicitation-dismiss-url", self.entry_ix), "Cancel")
                        .tab_index(0_isize)
                        .label_size(LabelSize::Small)
                        .on_click(move |_, window, cx| {
                            on_dismiss_url(dismiss_id.clone(), window, cx);
                        }),
                )
            })
            .into_any_element()
    }
}

/// The key suffix of the free-text answer that accompanies a select question,
/// as in Claude Code's `AskUserQuestion` forms.
const CUSTOM_ANSWER_SUFFIX: &str = "_custom";

/// One page of a tabbed questions card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QuestionTab {
    /// The index of the question's elicitation within the card.
    pub question_ix: usize,
    pub field_names: Vec<String>,
    pub label: SharedString,
    /// Shown above the fields when they don't describe the question themselves.
    pub prompt: Option<SharedString>,
}

fn is_select_property(property: &acp::ElicitationPropertySchema) -> bool {
    match property {
        acp::ElicitationPropertySchema::String(schema) => !single_select_options(schema).is_empty(),
        acp::ElicitationPropertySchema::Array(_) => true,
        _ => false,
    }
}

/// Splits pending form questions into tabs. A form made only of select
/// questions, each optionally followed by its free-text answer, gets a tab per
/// question; any other form keeps all of its fields on one tab.
pub(crate) fn question_tabs(questions: &[&Elicitation]) -> Vec<QuestionTab> {
    let mut tabs = Vec::new();
    for (question_ix, elicitation) in questions.iter().enumerate() {
        let acp::ElicitationMode::Form(mode) = &elicitation.request.mode else {
            continue;
        };
        let properties = &mode.requested_schema.properties;
        let message = SharedString::from(elicitation.request.message.clone());
        let selects = properties
            .iter()
            .filter(|(_, property)| is_select_property(property))
            .map(|(field_name, _)| field_name.clone())
            .collect::<Vec<_>>();
        let is_question_list = !selects.is_empty()
            && properties.iter().all(|(field_name, property)| {
                is_select_property(property)
                    || field_name
                        .strip_suffix(CUSTOM_ANSWER_SUFFIX)
                        .is_some_and(|question| selects.iter().any(|select| select == question))
            });
        if !is_question_list {
            tabs.push(QuestionTab {
                question_ix,
                field_names: properties.keys().cloned().collect(),
                label: mode
                    .requested_schema
                    .title
                    .clone()
                    .map(SharedString::from)
                    .unwrap_or_else(|| message.clone()),
                prompt: Some(message),
            });
            continue;
        }
        for field_name in selects {
            let custom_answer = format!("{field_name}{CUSTOM_ANSWER_SUFFIX}");
            let has_description = properties
                .get(&field_name)
                .and_then(property_description)
                .is_some();
            let label = properties
                .get(&field_name)
                .map(|property| property_title(&field_name, property))
                .unwrap_or_else(|| field_name.clone().into());
            let mut field_names = vec![field_name];
            if properties.contains_key(&custom_answer) {
                field_names.push(custom_answer);
            }
            tabs.push(QuestionTab {
                question_ix,
                field_names,
                label,
                // A lone question carries its text in the message instead.
                prompt: (!has_description).then(|| message.clone()),
            });
        }
    }
    tabs
}

pub(crate) struct QuestionForm<'a> {
    pub entry_ix: usize,
    pub elicitation: &'a Elicitation,
    pub form_state: Option<&'a ElicitationFormState>,
}

type QuestionsHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub(crate) struct QuestionsCardHandlers {
    pub on_select_tab: Rc<dyn Fn(usize, &mut App)>,
    pub on_submit_all: QuestionsHandler,
    pub on_decline_all: QuestionsHandler,
    pub on_cancel_all: QuestionsHandler,
}

/// The questions an agent is waiting on, one tab per question, ending with a
/// tab that summarizes the answers and submits them together.
pub(crate) struct QuestionsCard<'a> {
    questions: Vec<QuestionForm<'a>>,
    requester_name: SharedString,
    selected_tab: usize,
    field_handlers: ElicitationCardHandlers,
    handlers: QuestionsCardHandlers,
}

impl<'a> QuestionsCard<'a> {
    pub(crate) fn new(
        questions: Vec<QuestionForm<'a>>,
        requester_name: SharedString,
        selected_tab: usize,
        field_handlers: ElicitationCardHandlers,
        handlers: QuestionsCardHandlers,
    ) -> Self {
        Self {
            questions,
            requester_name,
            selected_tab,
            field_handlers,
            handlers,
        }
    }

    pub(crate) fn render(self, cx: &App) -> AnyElement {
        let elicitations = self
            .questions
            .iter()
            .map(|question| question.elicitation)
            .collect::<Vec<_>>();
        let tabs = question_tabs(&elicitations);
        if tabs.len() <= 1 {
            let Some(question) = self.questions.into_iter().next() else {
                return Empty.into_any_element();
            };
            return ElicitationCard::new(
                question.entry_ix,
                question.elicitation,
                self.requester_name,
                question.form_state,
                self.field_handlers,
            )
            .render(cx)
            .into_any_element();
        }

        let submit_tab = tabs.len();
        let selected_tab = self.selected_tab.min(submit_tab);
        let border_color = cx.theme().colors().border.opacity(0.8);
        let body = match tabs.get(selected_tab) {
            Some(tab) => self.render_question(tab, selected_tab, cx),
            None => self.render_summary(&tabs, cx),
        };

        v_flex()
            .key_context("Elicitation")
            .tab_group()
            .on_action(|_: &menu::SelectNext, window, cx| window.focus_next(cx))
            .on_action(|_: &menu::SelectPrevious, window, cx| window.focus_prev(cx))
            .on_key_down(self.key_down_handler(&tabs, selected_tab))
            .mx_5()
            .my_1p5()
            .rounded_md()
            .border_1()
            .border_color(border_color)
            .overflow_hidden()
            .child(self.render_tab_strip(&tabs, selected_tab, cx))
            .child(body)
            .child(self.render_footer(selected_tab, submit_tab, cx))
            .into_any_element()
    }

    fn render_tab_strip(&self, tabs: &[QuestionTab], selected_tab: usize, cx: &App) -> AnyElement {
        let submit_tab = tabs.len();
        let tab_button = |id: usize, label: SharedString, icon: Option<(IconName, Color)>| {
            let on_select_tab = self.handlers.on_select_tab.clone();
            div()
                .debug_selector(move || format!("question-tab-{id}"))
                .child(
                    Button::new(("question-tab", id), label)
                        .label_size(LabelSize::Small)
                        .toggle_state(id == selected_tab)
                        .when_some(icon, |button, (icon, color)| {
                            button.start_icon(Icon::new(icon).size(IconSize::XSmall).color(color))
                        })
                        .on_click(move |_, _, cx| on_select_tab(id, cx)),
                )
        };

        h_flex()
            .id("question-tabs")
            .w_full()
            .p_1()
            .gap_0p5()
            .overflow_x_scroll()
            .border_b_1()
            .border_color(cx.theme().colors().border.opacity(0.8))
            .bg(cx
                .theme()
                .colors()
                .element_background
                .blend(cx.theme().colors().editor_foreground.opacity(0.025)))
            .children(tabs.iter().enumerate().map(|(ix, tab)| {
                let form_state = self.questions[tab.question_ix].form_state;
                let icon = if form_state.is_some_and(|state| state.has_errors(&tab.field_names)) {
                    Some((IconName::Warning, Color::Error))
                } else if form_state.is_some_and(|state| state.is_answered(&tab.field_names, cx)) {
                    Some((IconName::Check, Color::Success))
                } else {
                    None
                };
                tab_button(ix, tab.label.clone(), icon)
            }))
            .child(tab_button(submit_tab, "Submit".into(), None))
            .into_any_element()
    }

    fn render_question(&self, tab: &QuestionTab, selected_tab: usize, cx: &App) -> AnyElement {
        let question = &self.questions[tab.question_ix];
        let on_select_tab = self.handlers.on_select_tab.clone();
        let field_handlers = self
            .field_handlers
            .clone()
            .after_single_select(move |cx| on_select_tab(selected_tab + 1, cx));
        let card = ElicitationCard::new(
            question.entry_ix,
            question.elicitation,
            self.requester_name.clone(),
            question.form_state,
            field_handlers,
        );
        v_flex()
            .p_3()
            .gap_2()
            .when_some(tab.prompt.clone(), |this, prompt| {
                this.child(Label::new(prompt).size(LabelSize::Small))
            })
            .child(card.render_fields(&tab.field_names, cx))
            .into_any_element()
    }

    fn render_summary(&self, tabs: &[QuestionTab], cx: &App) -> AnyElement {
        v_flex()
            .debug_selector(|| "questions-summary".to_string())
            .p_3()
            .gap_1()
            .children(tabs.iter().map(|tab| {
                let answer = self.questions[tab.question_ix]
                    .form_state
                    .and_then(|state| state.answer_summary(&tab.field_names, cx));
                let is_answered = answer.is_some();
                h_flex()
                    .gap_2()
                    .child(
                        Label::new(tab.label.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(answer.unwrap_or_else(|| "Skipped".into()))
                            .size(LabelSize::Small)
                            .color(if is_answered {
                                Color::Default
                            } else {
                                Color::Muted
                            })
                            .truncate(),
                    )
            }))
            .into_any_element()
    }

    fn render_footer(&self, selected_tab: usize, submit_tab: usize, cx: &App) -> AnyElement {
        let is_submitting = self
            .questions
            .iter()
            .any(|question| question.form_state.is_some_and(|state| state.is_submitting));
        let footer = h_flex()
            .w_full()
            .p_1()
            .gap_1()
            .justify_end()
            .border_t_1()
            .border_color(cx.theme().colors().border.opacity(0.8));
        if selected_tab < submit_tab {
            let on_back = self.handlers.on_select_tab.clone();
            let on_next = self.handlers.on_select_tab.clone();
            return footer
                .child(
                    Button::new("question-back", "Back")
                        .label_size(LabelSize::Small)
                        .disabled(selected_tab == 0)
                        .on_click(move |_, _, cx| on_back(selected_tab.saturating_sub(1), cx)),
                )
                .child(
                    Button::new("question-next", "Next")
                        .label_size(LabelSize::Small)
                        .on_click(move |_, _, cx| on_next(selected_tab + 1, cx)),
                )
                .into_any_element();
        }
        let on_submit_all = self.handlers.on_submit_all.clone();
        let on_decline_all = self.handlers.on_decline_all.clone();
        let on_cancel_all = self.handlers.on_cancel_all.clone();
        footer
            .child(
                Button::new("questions-submit", "Submit")
                    .tab_index(0_isize)
                    .start_icon(
                        Icon::new(IconName::Check)
                            .size(IconSize::XSmall)
                            .color(Color::Success),
                    )
                    .label_size(LabelSize::Small)
                    .disabled(is_submitting)
                    .on_click(move |_, window, cx| on_submit_all(window, cx)),
            )
            .child(
                Button::new("questions-decline", "Decline")
                    .tab_index(0_isize)
                    .start_icon(
                        Icon::new(IconName::Close)
                            .size(IconSize::XSmall)
                            .color(Color::Error),
                    )
                    .label_size(LabelSize::Small)
                    .on_click(move |_, window, cx| on_decline_all(window, cx)),
            )
            .child(
                Button::new("questions-cancel", "Cancel")
                    .tab_index(0_isize)
                    .label_size(LabelSize::Small)
                    .on_click(move |_, window, cx| on_cancel_all(window, cx)),
            )
            .into_any_element()
    }

    /// ←/→ switch questions and 1–9 pick an option, unless a text field has
    /// focus and needs those keys itself.
    fn key_down_handler(
        &self,
        tabs: &[QuestionTab],
        selected_tab: usize,
    ) -> impl Fn(&gpui::KeyDownEvent, &mut Window, &mut App) + 'static {
        let submit_tab = tabs.len();
        let tab = tabs.get(selected_tab);
        let question = tab.map(|tab| &self.questions[tab.question_ix]);
        let text_fields = tab
            .zip(question.and_then(|question| question.form_state))
            .map(|(tab, state)| {
                tab.field_names
                    .iter()
                    .filter_map(|field_name| match state.fields.get(field_name) {
                        Some(ElicitationFieldState::Text(editor)) => Some(editor.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let single_select = tab.zip(question).and_then(|(tab, question)| {
            let acp::ElicitationMode::Form(mode) = &question.elicitation.request.mode else {
                return None;
            };
            let field_name = tab.field_names.first()?;
            let acp::ElicitationPropertySchema::String(schema) =
                mode.requested_schema.properties.get(field_name)?
            else {
                return None;
            };
            let values = single_select_options(schema)
                .into_iter()
                .map(|option| option.value)
                .collect::<Vec<_>>();
            (!values.is_empty())
                .then(|| (question.elicitation.id.clone(), field_name.clone(), values))
        });
        let on_select_tab = self.handlers.on_select_tab.clone();
        let on_single_select_change = self.field_handlers.on_single_select_change.clone();

        move |event, window, cx| {
            if text_fields
                .iter()
                .any(|editor| editor.focus_handle(cx).is_focused(window))
            {
                return;
            }
            let keystroke = &event.keystroke;
            if keystroke.modifiers.modified() {
                return;
            }
            match keystroke.key.as_str() {
                "left" if selected_tab > 0 => {
                    on_select_tab(selected_tab - 1, cx);
                    cx.stop_propagation();
                }
                "right" if selected_tab < submit_tab => {
                    on_select_tab(selected_tab + 1, cx);
                    cx.stop_propagation();
                }
                key => {
                    let Some((elicitation_id, field_name, values)) = &single_select else {
                        return;
                    };
                    let Some(value) = key
                        .parse::<usize>()
                        .ok()
                        .and_then(|number| number.checked_sub(1))
                        .and_then(|index| values.get(index))
                    else {
                        return;
                    };
                    on_single_select_change(
                        elicitation_id.clone(),
                        field_name.clone(),
                        value.clone(),
                        cx,
                    );
                    on_select_tab(selected_tab + 1, cx);
                    cx.stop_propagation();
                }
            }
        }
    }
}
