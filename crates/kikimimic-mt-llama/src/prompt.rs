//! Prompt formats, rendered on the host so the dylib stays a plain generator
//! and a new model family is one variant here.

use kikimimic_core::engine::MtRequest;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptFormat {
    /// Instruction-tuned chat models (Qwen3-Instruct and friends).
    ChatMl,
    /// PLaMo 3 Translate's PFML3, as PFN's `chat_template.jinja` renders it.
    Plamo3,
}

impl PromptFormat {
    /// The catalog's `options.prompt` value.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "chatml" => Some(Self::ChatMl),
            "plamo3" => Some(Self::Plamo3),
            _ => None,
        }
    }

    pub fn render(&self, req: &MtRequest<'_>) -> String {
        match self {
            Self::ChatMl => chatml(req),
            Self::Plamo3 => plamo3(req),
        }
    }
}

/// v2's interpreter prompt. Context is left out: a chat model told about
/// earlier lines tends to translate them again.
fn chatml(req: &MtRequest<'_>) -> String {
    let target = language_name(req.target.as_str());
    // An unconfident source is not named: told the wrong language, the model
    // "corrects" the sentence into it instead of translating it.
    let from = req
        .source
        .map(|l| format!("from {} ", language_name(l.as_str())))
        .unwrap_or_default();
    let mut system = format!(
        "You are a professional simultaneous interpreter. \
         Translate the user's sentence {from}to {target}. \
         If it is already in {target}, repeat it unchanged. \
         Output only the translation, nothing else."
    );
    if !req.terms.is_empty() {
        system.push_str(&format!(
            " Keep these terms exactly as written: {}.",
            req.terms.join(", ")
        ));
    }
    format!(
        "<|im_start|>system\n{system}<|im_end|>\n\
         <|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
        req.text
    )
}

/// PFML3 for one sentence, following PFN's chat_template.jinja: dataset,
/// optional context, optional glossary, then input and the output header.
fn plamo3(req: &MtRequest<'_>) -> String {
    let mut p = String::from("<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation");
    if !req.context.is_empty() {
        p += "<|plamo:tag|>context<|plamo:msg|>";
        p += &clean(&req.context.join("\n"));
    }
    if !req.terms.is_empty() {
        p += "<|plamo:tag|>glossary";
        for term in req.terms {
            // Our terms are spellings to keep, so a term maps to itself.
            let term = clean(term);
            p += &format!("<|plamo:index|>{term}<|plamo:item|>{term}");
        }
    }
    p += "<|plamo:tag|>input";
    if let Some(source) = req.source {
        p += &format!("<|plamo:key|>lang<|plamo:val|>{source}");
    }
    p += &format!(
        "<|plamo:msg|>{}<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>{}<|plamo:msg|>",
        clean(req.text),
        req.target
    );
    p
}

/// The template refuses text containing reserved tokens; transcripts are
/// untrusted, so strip them instead of failing the sentence.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<|plamo:") {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        rest = rest.find("|>").map_or("", |end| &rest[end + 2..]);
    }
    out.push_str(rest);
    out
}

fn language_name(code: &str) -> &str {
    kikimimic_core::language::language_name(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kikimimic_core::domain::Lang;

    fn lang(c: &str) -> Lang {
        Lang::parse(c).unwrap()
    }

    fn req<'a>(
        text: &'a str,
        source: Option<&'a Lang>,
        target: &'a Lang,
        context: &'a [String],
        terms: &'a [String],
    ) -> MtRequest<'a> {
        MtRequest {
            text,
            source,
            target,
            context,
            terms,
        }
    }

    #[test]
    fn plamo3_matches_the_reference_template_for_a_bare_request() {
        let (en, ja) = (lang("en"), lang("ja"));
        assert_eq!(
            PromptFormat::Plamo3.render(&req("Hello.", Some(&en), &ja, &[], &[])),
            "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation\
             <|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Hello.\
             <|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>"
        );
    }

    #[test]
    fn plamo3_places_context_and_glossary_where_the_template_does() {
        let ja = lang("ja");
        let context = ["First line.".to_string(), "Second line.".to_string()];
        let terms = ["Kikimimic".to_string()];
        assert_eq!(
            PromptFormat::Plamo3.render(&req("Third.", None, &ja, &context, &terms)),
            "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation\
             <|plamo:tag|>context<|plamo:msg|>First line.\nSecond line.\
             <|plamo:tag|>glossary<|plamo:index|>Kikimimic<|plamo:item|>Kikimimic\
             <|plamo:tag|>input<|plamo:msg|>Third.\
             <|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>"
        );
    }

    #[test]
    fn plamo3_strips_reserved_tokens_from_user_text() {
        let ja = lang("ja");
        let rendered = PromptFormat::Plamo3.render(&req("a<|plamo:eos|>b", None, &ja, &[], &[]));
        assert!(
            rendered.contains("<|plamo:msg|>ab<|plamo:tag|>output"),
            "{rendered}"
        );
    }

    #[test]
    fn chatml_names_languages_and_keeps_terms() {
        let (en, ja) = (lang("en"), lang("ja"));
        let terms = ["Kikimimic".to_string()];
        let p = PromptFormat::ChatMl.render(&req("Hello.", Some(&en), &ja, &[], &terms));
        assert!(p.starts_with("<|im_start|>system\n"), "{p}");
        assert!(p.contains("from English to Japanese"), "{p}");
        assert!(p.contains("Kikimimic"), "{p}");
        assert!(
            p.ends_with("<|im_start|>user\nHello.<|im_end|>\n<|im_start|>assistant\n"),
            "{p}"
        );
    }

    #[test]
    fn chatml_names_no_source_when_detection_was_unconfident() {
        let ja = lang("ja");
        let p = PromptFormat::ChatMl.render(&req("Hello.", None, &ja, &[], &[]));
        assert!(
            p.contains("Translate the user's sentence to Japanese"),
            "{p}"
        );
    }
}
