//! Limits on what one evaluation may create.
//!
//! Filters have no loops, but some operations produce far more data than
//! the filter text that asks for them: every `.replace("a", "aaaaaaaaaa")`
//! multiplies a string's length by ten, `range()` builds arrays, `+`
//! concatenates, and `string()` writes whole arrays out. A short filter
//! could otherwise make an evaluation allocate without bound and abort the
//! process. Every evaluation therefore gets a [`Budget`] for the text and
//! for the array items and dictionary entries that operations create; an
//! operation that would exceed it fails with an error before allocating.
//!
//! Values read from the scope and literals written in the filter don't
//! count: they exist already, and the filter's length bounds how often it
//! can refer to them.

use std::borrow::Cow;
use std::cell::Cell;

use crate::value::{Value, format_number};

/// Bytes of text one evaluation may create. Far more than filters on large
/// plugin outputs need (a few copies of the output), small enough that no
/// filter can exhaust memory.
pub(crate) const MAX_TEXT: usize = 16 * 1024 * 1024;

/// Array items and dictionary entries one evaluation may create.
pub(crate) const MAX_ITEMS: usize = 100_000;

/// What is left of one evaluation's limits. Operations charge what they
/// are about to create; the first charge that doesn't fit fails.
#[derive(Debug)]
pub(crate) struct Budget {
    text: Cell<usize>,
    items: Cell<usize>,
}

impl Budget {
    /// A full budget, for one evaluation.
    pub(crate) fn new() -> Self {
        Budget {
            text: Cell::new(MAX_TEXT),
            items: Cell::new(MAX_ITEMS),
        }
    }

    /// Charges `bytes` of new text.
    pub(crate) fn text(&self, bytes: usize) -> Result<(), String> {
        let left = self.text.get();
        if bytes > left {
            return Err(too_much_text());
        }
        self.text.set(left - bytes);
        Ok(())
    }

    /// Charges `count` new array items or dictionary entries.
    pub(crate) fn items(&self, count: usize) -> Result<(), String> {
        let left = self.items.get();
        if count > left {
            return Err(too_many_items());
        }
        self.items.set(left - count);
        Ok(())
    }

    /// Bytes of text that may still be created.
    pub(crate) fn text_left(&self) -> usize {
        self.text.get()
    }

    /// The text of a value as `string()` converts it. Strings are borrowed;
    /// converting anything else is charged, and an array or dictionary too
    /// large to write out fails without being written out completely.
    pub(crate) fn text_of<'v>(&self, value: &'v Value) -> Result<Cow<'v, str>, String> {
        match value {
            Value::String(text) => Ok(Cow::Borrowed(text)),
            Value::Null => Ok(Cow::Borrowed("")),
            Value::Bool(true) => Ok(Cow::Borrowed("true")),
            Value::Bool(false) => Ok(Cow::Borrowed("false")),
            Value::Number(number) => {
                let text = format_number(*number);
                self.text(text.len())?;
                Ok(Cow::Owned(text))
            }
            Value::Array(_) | Value::Dict(_) => {
                let text = value
                    .icinga_string_within(self.text_left())
                    .ok_or_else(too_much_text)?;
                self.text(text.len())?;
                Ok(Cow::Owned(text))
            }
        }
    }
}

#[cold]
fn too_much_text() -> String {
    format!(
        "evaluation limit reached: the filter would create more than {} MiB of text",
        MAX_TEXT / (1024 * 1024)
    )
}

#[cold]
fn too_many_items() -> String {
    format!(
        "evaluation limit reached: the filter would create more than {MAX_ITEMS} array items \
         and dictionary entries"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_until_the_limit() {
        let budget = Budget::new();
        budget.text(MAX_TEXT - 10).unwrap();
        budget.text(10).unwrap();
        assert!(budget.text(1).unwrap_err().contains("16 MiB of text"));
        budget.items(MAX_ITEMS).unwrap();
        assert!(
            budget
                .items(1)
                .unwrap_err()
                .contains("100000 array items and dictionary entries")
        );
        assert_eq!(budget.text_left(), 0);
    }

    #[test]
    fn text_of_borrows_strings_and_charges_conversions() {
        let budget = Budget::new();
        assert!(matches!(
            budget.text_of(&Value::from("x")).unwrap(),
            Cow::Borrowed("x")
        ));
        assert_eq!(budget.text_of(&Value::Null).unwrap(), "");
        assert_eq!(budget.text_of(&Value::Bool(true)).unwrap(), "true");
        assert_eq!(budget.text_of(&Value::Number(2.5)).unwrap(), "2.500000");
        assert_eq!(budget.text_left(), MAX_TEXT - "2.500000".len());
        let array = Value::from(vec![Value::from("a"), Value::Number(1.0)]);
        assert_eq!(budget.text_of(&array).unwrap(), r#"[ "a", 1.000000 ]"#);

        let small = Budget::new();
        small.text(MAX_TEXT - 5).unwrap();
        assert!(small.text_of(&array).is_err(), "too long to write out");
        assert_eq!(small.text_left(), 5, "a failed conversion charges nothing");
    }
}
