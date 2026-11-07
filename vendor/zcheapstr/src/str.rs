#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use alloc::{borrow::Cow, string::String, sync::Arc};
use core::{
    cmp::Ordering,
    hash::{Hash, Hasher},
    ptr::NonNull,
};

/// A string wrapper.
///
///
/// This is similar to the [`Cow`] type, but it:
///
/// * is specialized for strings.
/// * treats `&'static str` as a separate type. This allows you to avoid allocations and copying
///   when turning a `CheapStr` instance created from a `&'static str` into an owned version in
///   generic code that doesn't/can't assume the inner lifetime of the source `CheapStr` instance.
/// * stores owned strings in an [`Arc`], so `Clone` never copies or allocates: it either copies a
///   reference or increments a reference count.
/// * is immutable. Consequently, unlike [`Cow`], it is *not* a copy-on-write type: there is no way
///   to get a mutable reference to the underlying string.
///
/// API is provided to convert from, and to a [`&str`] and [`String`].
///
/// [`&str`]: https://doc.rust-lang.org/std/str/index.html
/// [`String`]: https://doc.rust-lang.org/std/string/struct.String.html
#[derive(Default, PartialEq, Eq, PartialOrd, Ord, Hash, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CheapStr<'a>(#[cfg_attr(feature = "serde", serde(borrow))] Inner<'a>);

#[derive(Clone)]
enum Inner<'a> {
    Static(&'static str),
    Borrowed(&'a str),
    Owned(ArcStr),
}

impl Default for Inner<'_> {
    fn default() -> Self {
        Self::Static("")
    }
}

impl<'a> PartialEq for Inner<'a> {
    fn eq(&self, other: &Inner<'a>) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Inner<'_> {}

impl<'a> Ord for Inner<'a> {
    fn cmp(&self, other: &Inner<'a>) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl<'a> PartialOrd for Inner<'a> {
    fn partial_cmp(&self, other: &Inner<'a>) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for Inner<'_> {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.as_str().hash(h)
    }
}

impl Inner<'_> {
    /// The underlying string.
    pub const fn as_str(&self) -> &str {
        match self {
            Inner::Static(s) => s,
            Inner::Borrowed(s) => s,
            Inner::Owned(s) => s.as_str(),
        }
    }
}

#[cfg(feature = "serde")]
impl Serialize for Inner<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

#[cfg(feature = "serde")]
impl<'de: 'a, 'a> Deserialize<'de> for Inner<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        <&'a str>::deserialize(deserializer).map(Inner::Borrowed)
    }
}

impl CheapStr<'_> {
    /// An owned string without allocations
    pub const fn from_static(s: &'static str) -> Self {
        CheapStr(Inner::Static(s))
    }

    /// This is faster than `Clone::clone` when `self` contains owned data.
    pub fn as_ref(&self) -> CheapStr<'_> {
        match &self.0 {
            Inner::Static(s) => CheapStr(Inner::Static(s)),
            Inner::Borrowed(s) => CheapStr(Inner::Borrowed(s)),
            Inner::Owned(s) => CheapStr(Inner::Borrowed(s.as_str())),
        }
    }

    /// The underlying string.
    ///
    /// This is a `const fn`. As `CheapStr` has a destructor, which can't run in `const` context,
    /// `const` code can only call it on an instance that is never dropped, such as one behind a
    /// `const` reference or in a `static`:
    ///
    /// ```
    /// use zcheapstr::CheapStr;
    ///
    /// const NAME: &CheapStr<'static> = &CheapStr::from_static("zcheapstr");
    /// const NAME_STR: &str = NAME.as_str();
    /// assert_eq!(NAME_STR, "zcheapstr");
    /// ```
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Creates an owned clone of `self`.
    pub fn to_owned(&self) -> CheapStr<'static> {
        self.clone().into_owned()
    }

    /// Creates an owned clone of `self`.
    pub fn into_owned(self) -> CheapStr<'static> {
        match self.0 {
            Inner::Static(s) => CheapStr(Inner::Static(s)),
            Inner::Borrowed(s) => CheapStr::from(Arc::<str>::from(s)),
            Inner::Owned(s) => CheapStr(Inner::Owned(s)),
        }
    }
}

impl<'a> From<&'a str> for CheapStr<'a> {
    fn from(value: &'a str) -> Self {
        Self(Inner::Borrowed(value))
    }
}

impl<'a> From<&'a String> for CheapStr<'a> {
    fn from(value: &'a String) -> Self {
        Self(Inner::Borrowed(value))
    }
}

impl From<String> for CheapStr<'_> {
    fn from(value: String) -> Self {
        Self::from(Arc::<str>::from(value))
    }
}

impl From<Arc<str>> for CheapStr<'_> {
    fn from(value: Arc<str>) -> Self {
        Self(Inner::Owned(ArcStr::from(value)))
    }
}

impl<'a> From<Cow<'a, str>> for CheapStr<'a> {
    fn from(value: Cow<'a, str>) -> Self {
        match value {
            Cow::Owned(value) => value.into(),
            Cow::Borrowed(value) => value.into(),
        }
    }
}

impl<'a> From<CheapStr<'a>> for String {
    fn from(value: CheapStr<'a>) -> String {
        match value.0 {
            Inner::Static(s) => s.into(),
            Inner::Borrowed(s) => s.into(),
            Inner::Owned(s) => s.as_str().into(),
        }
    }
}

impl<'a> From<&'a CheapStr<'_>> for &'a str {
    fn from(value: &'a CheapStr<'_>) -> &'a str {
        value.as_str()
    }
}

impl core::ops::Deref for CheapStr<'_> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl PartialEq<str> for CheapStr<'_> {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for CheapStr<'_> {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl core::fmt::Debug for CheapStr<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl core::fmt::Display for CheapStr<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self.as_str(), f)
    }
}

/// An `Arc<str>`, held as the pointer to its string.
///
/// `Arc` offers no way to reach its data in `const` context, while a pointer can be dereferenced
/// there. Holding the pointer that `Arc::into_raw` returns, instead of the `Arc` itself, is what
/// allows [`CheapStr::as_str`] to be a `const fn`.
///
/// The unsafe code below relies on this invariant: the pointer is one that `Arc::<str>::into_raw`
/// returned, and the `ArcStr` owns one strong reference to that `Arc`. `From` sets it up, taking
/// over the reference of the `Arc` it is given, `Clone` adds a reference for the clone to own and
/// `Drop` gives the reference back. Nothing else creates an `ArcStr`, changes its pointer or
/// touches the reference counts, and as neither the type nor its field is visible outside of this
/// file, no other code can.
struct ArcStr(NonNull<str>);

impl ArcStr {
    const fn as_str(&self) -> &str {
        // SAFETY: By the invariant, the pointer points to the string of an `Arc` that `self` owns
        // a strong reference to. The returned reference borrows `self`, so `self` and its strong
        // reference outlive it, which keeps the string allocated. Nothing can change the string
        // meanwhile either: `str` has no interior mutability, and `Arc` only grants mutable access
        // to its data when no other `Arc` or `Weak` points to the same allocation, which the strong
        // reference of `self` rules out.
        unsafe { self.0.as_ref() }
    }
}

impl From<Arc<str>> for ArcStr {
    fn from(value: Arc<str>) -> Self {
        // `into_raw` doesn't give back the strong reference of `value`: the `ArcStr` takes it over.
        let ptr = Arc::into_raw(value).cast_mut();
        // SAFETY: `Arc::into_raw` never returns a null pointer, as it points to the data inside the
        // allocation of the `Arc`.
        Self(unsafe { NonNull::new_unchecked(ptr) })
    }
}

impl Clone for ArcStr {
    fn clone(&self) -> Self {
        // SAFETY: By the invariant, the pointer is one that `Arc::<str>::into_raw` returned, so it
        // has the layout that `Arc<str>` requires and points into memory of the global allocator,
        // which `Arc<str>` uses. The strong reference of `self` keeps that `Arc` alive throughout.
        // The clone owns the reference that this adds, which upholds the invariant for it.
        unsafe { Arc::increment_strong_count(self.0.as_ptr()) };

        Self(self.0)
    }
}

impl Drop for ArcStr {
    fn drop(&mut self) {
        // SAFETY: By the invariant, the pointer is one that `Arc::<str>::into_raw` returned, and
        // `self` owns a strong reference to that `Arc`. The rebuilt `Arc` takes that reference over
        // and gives it back as it is dropped here. As `self` is never used again, and every
        // `ArcStr` owns a reference of its own, each reference is given back exactly once.
        drop(unsafe { Arc::from_raw(self.0.as_ptr()) });
    }
}

// SAFETY: An `ArcStr` uses its pointer only to read the string, which can't change (see
// `ArcStr::as_str`), and to change the reference counts of the `Arc`, which `Arc` does atomically.
// So it can be sent to, and dropped on, another thread like the `Arc<str>` it stands for, which is
// `Send` as `str` is `Send` and `Sync`.
unsafe impl Send for ArcStr {}

// SAFETY: A shared `ArcStr` can only be read, or cloned, which adds a strong reference atomically.
// So it can be shared between threads like the `Arc<str>` it stands for, which is `Sync` as `str`
// is `Send` and `Sync`.
unsafe impl Sync for ArcStr {}

#[cfg(test)]
mod tests {
    use super::CheapStr;
    use alloc::{
        string::{String, ToString},
        sync::Arc,
    };
    use core::panic::{RefUnwindSafe, UnwindSafe};

    #[test]
    fn from_string() {
        let string = String::from("value");
        let v = CheapStr::from(&string);
        assert_eq!(v.as_str(), "value");
    }

    #[test]
    fn test_ordering() {
        let first = CheapStr::from("a".to_string());
        let second = CheapStr::from_static("b");
        assert!(first < second);
    }

    #[test]
    fn as_str_in_const() {
        const STATIC: &CheapStr<'static> = &CheapStr::from_static("static");
        const STATIC_STR: &str = STATIC.as_str();
        assert_eq!(STATIC_STR, "static");
    }

    #[test]
    fn shared_data() {
        let arc = Arc::<str>::from("shared");
        let s = CheapStr::from(arc.clone());
        assert_eq!(s.as_str(), "shared");
        assert_eq!(Arc::strong_count(&arc), 2);

        // Neither cloning nor turning into an owned instance copies the string.
        let clone = s.clone();
        assert_eq!(Arc::strong_count(&arc), 3);
        assert!(core::ptr::eq(clone.as_str(), &*arc));
        let owned = s.into_owned();
        assert_eq!(Arc::strong_count(&arc), 3);
        assert!(core::ptr::eq(owned.as_str(), &*arc));

        drop(clone);
        assert_eq!(String::from(owned), "shared");
        assert_eq!(Arc::strong_count(&arc), 1);
    }

    #[test]
    fn auto_traits() {
        fn assert_auto_traits<T>()
        where
            T: Send + Sync + Unpin + UnwindSafe + RefUnwindSafe,
        {
        }

        assert_auto_traits::<CheapStr<'_>>();
    }
}

#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use super::CheapStr;
    use alloc::string::String;

    #[test]
    fn serde_round_trip() {
        let s = CheapStr::from("hello");
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"hello\"");
        let deserialized: CheapStr<'_> = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, s);

        // Owned data serializes the same way.
        let owned = CheapStr::from(String::from("hello"));
        assert_eq!(serde_json::to_string(&owned).unwrap(), json);
    }

    #[test]
    fn serde_non_borrowable_input_errors() {
        // `CheapStr` only supports borrowed deserialization: input that cannot be handed out as a
        // borrowed `&str` (here because of the escape sequence) is an error, not an allocation.
        serde_json::from_str::<CheapStr<'_>>("\"a\\nb\"").unwrap_err();
    }

    #[test]
    fn serde_borrowed_deserialization() {
        let json = String::from("\"borrowed\"");
        let s: CheapStr<'_> = serde_json::from_str(&json).unwrap();
        assert_eq!(s.as_str(), "borrowed");
        // The deserialized `CheapStr` borrows from the JSON input instead of allocating.
        assert!(core::ptr::eq(s.as_str().as_ptr(), json[1..].as_ptr()));
    }
}
