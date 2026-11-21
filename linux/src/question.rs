use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Weak;

use adw::prelude::*;
use local_studio_core::Question;

use crate::app::App;
use crate::icons;
use crate::state::State;

#[derive(Clone)]
pub struct Pending {
    pub request: String,
    pub question: Question,
}

pub struct QuestionCard {
    pub root: gtk::Box,
    plugin: gtk::Label,
    count: gtk::Label,
    text: gtk::Label,
    choices: gtk::FlowBox,
    entry: gtk::Entry,
    shown: RefCell<Option<Pending>>,
    app: Weak<App>,
}

impl QuestionCard {
    pub fn new(app: &Weak<App>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.add_css_class("ls-approval");
        root.add_css_class("ls-question");
        root.set_visible(false);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let plugin = gtk::Label::new(None);
        plugin.add_css_class("kind");
        plugin.set_xalign(0.0);
        plugin.set_hexpand(true);
        let count = gtk::Label::new(None);
        count.add_css_class("ls-meta");
        top.append(&icons::styled(icons::PROVIDERS, "ls-question-icon"));
        top.append(&plugin);
        top.append(&count);
        let text = gtk::Label::new(None);
        text.set_xalign(0.0);
        text.set_wrap(true);
        text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        text.set_selectable(true);
        let choices = gtk::FlowBox::new();
        choices.set_selection_mode(gtk::SelectionMode::None);
        choices.set_column_spacing(8);
        choices.set_row_spacing(6);
        choices.set_max_children_per_line(8);
        let answer_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let entry = gtk::Entry::new();
        entry.set_hexpand(true);
        let send = gtk::Button::with_label("Answer");
        send.add_css_class("flat");
        answer_row.append(&entry);
        answer_row.append(&send);
        root.append(&top);
        root.append(&text);
        root.append(&choices);
        root.append(&answer_row);
        let weak = app.clone();
        entry.connect_activate(move |entry| answer_typed(&weak, entry));
        let weak = app.clone();
        let typed = entry.clone();
        send.connect_clicked(move |_| answer_typed(&weak, &typed));
        Self { root, plugin, count, text, choices, entry, shown: RefCell::new(None), app: app.clone() }
    }

    pub fn render(&self, state: &State) {
        let list = state.selected.as_ref().and_then(|thread| state.questions.get(thread));
        let first = list.and_then(|list| list.first()).cloned();
        self.root.set_visible(first.is_some());
        let Some(pending) = first else {
            self.shown.replace(None);
            return;
        };
        let changed = self.shown.borrow().as_ref().is_none_or(|shown| shown.request != pending.request);
        let total = list.map_or(0, Vec::len);
        self.count.set_label(&format!("1/{total}"));
        self.count.set_visible(total > 1);
        if !changed {
            return;
        }
        let question = &pending.question;
        self.plugin.set_label(&format!("{} asks", question.header));
        self.text.set_label(&question.question);
        self.entry.set_text("");
        self.entry.set_placeholder_text(Some(if question.options.is_empty() {
            "Type your answer"
        } else {
            "Or type another answer"
        }));
        self.choices.remove_all();
        self.choices.set_visible(!question.options.is_empty());
        for option in &question.options {
            let button = gtk::Button::with_label(&option.label);
            if !option.description.is_empty() {
                button.set_tooltip_text(Some(&option.description));
            }
            let weak = self.app.clone();
            let label = option.label.clone();
            button.connect_clicked(move |_| answer(&weak, &label));
            self.choices.append(&button);
        }
        self.shown.replace(Some(pending));
    }
}

fn answer_typed(app: &Weak<App>, entry: &gtk::Entry) {
    let text = entry.text().trim().to_string();
    if !text.is_empty() {
        answer(app, &text);
    }
}

fn answer(app: &Weak<App>, text: &str) {
    let Some(app) = app.upgrade() else { return };
    let thread = app.state.borrow().selected.clone();
    let Some(pending) = app.composer.question.shown.borrow().clone() else { return };
    let Some(thread) = thread else { return };
    let answers = HashMap::from([(pending.question.question.clone(), text.to_string())]);
    app.core.run(move |studio| studio.answer_questions(thread, pending.request, answers));
}

pub fn set(questions: &mut HashMap<String, Vec<Pending>>, thread: String, request: String, list: Vec<Question>) {
    let Some(question) = list.into_iter().next() else { return };
    let pending = questions.entry(thread).or_default();
    let fresh = Pending { request, question };
    match pending.iter_mut().find(|known| known.request == fresh.request) {
        Some(known) => *known = fresh,
        None => pending.push(fresh),
    }
}

pub fn settle(questions: &mut HashMap<String, Vec<Pending>>, thread: &str, request: &str) {
    if let Some(list) = questions.get_mut(thread) {
        list.retain(|pending| pending.request != request);
        if list.is_empty() {
            questions.remove(thread);
        }
    }
}
