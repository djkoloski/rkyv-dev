//! [`Archive`] implementation for shared pointers.

#[cfg(feature = "validation")]
pub mod validation;

use core::{
    cmp::PartialEq,
    mem::{forget, MaybeUninit},
    ops::Deref,
    pin::Pin,
    ptr,
};
use std::{rc, sync};

use crate::{
    de::{SharedDeserializer, SharedPointer},
    ser::SharedSerializer,
    Archive, ArchivePointee, ArchiveUnsized, Archived, Deserialize, DeserializeUnsized, RelPtr,
    Serialize, SerializeUnsized,
};

impl<T: ?Sized> SharedPointer for rc::Rc<T> {
    #[inline]
    fn data_address(&self) -> *const () {
        rc::Rc::as_ptr(self) as *const ()
    }
}

/// The resolver for `Rc`.
pub struct RcResolver<T> {
    pos: usize,
    metadata_resolver: T,
}

/// An archived `Rc`.
///
/// This is a thin wrapper around a [`RelPtr`] to the archived type.
#[repr(transparent)]
pub struct ArchivedRc<T: ArchivePointee + ?Sized>(RelPtr<T>);

impl<T: ArchivePointee + ?Sized> ArchivedRc<T> {
    /// Gets the value of this archived `Rc`.
    ///
    /// # Safety
    ///
    /// Any other `ArchivedRc` pointers to the same value must not be dereferenced for the duration
    /// of the returned borrow.
    pub unsafe fn get_pin_unchecked(self: Pin<&mut Self>) -> Pin<&mut T> {
        self.map_unchecked_mut(|s| &mut *s.0.as_mut_ptr())
    }
}

impl<T: ArchivePointee + ?Sized> Deref for ArchivedRc<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0.as_ptr() }
    }
}

impl<T: ArchivePointee + PartialEq<U> + ?Sized, U: ?Sized> PartialEq<rc::Rc<U>> for ArchivedRc<T> {
    #[inline]
    fn eq(&self, other: &rc::Rc<U>) -> bool {
        self.deref().eq(other.deref())
    }
}

impl<T: ArchiveUnsized + ?Sized> Archive for rc::Rc<T> {
    type Archived = ArchivedRc<T::Archived>;
    type Resolver = RcResolver<T::MetadataResolver>;

    #[inline]
    unsafe fn resolve(&self, pos: usize, resolver: Self::Resolver, out: &mut MaybeUninit<Self::Archived>) {
        let (fp, fo) = out_field!(out.0);
        self.as_ref()
            .resolve_unsized(pos + fp, resolver.pos, resolver.metadata_resolver, fo);
    }
}

impl<T: SerializeUnsized<S> + ?Sized + 'static, S: SharedSerializer + ?Sized> Serialize<S>
    for rc::Rc<T>
{
    #[inline]
    fn serialize(&self, serializer: &mut S) -> Result<Self::Resolver, S::Error> {
        Ok(RcResolver {
            pos: serializer.serialize_shared(self.deref())?,
            metadata_resolver: self.deref().serialize_metadata(serializer)?,
        })
    }
}

impl<T: ArchiveUnsized + ?Sized + 'static, D: SharedDeserializer + ?Sized> Deserialize<rc::Rc<T>, D>
    for Archived<rc::Rc<T>>
where
    T::Archived: DeserializeUnsized<T, D>,
{
    #[inline]
    fn deserialize(&self, deserializer: &mut D) -> Result<rc::Rc<T>, D::Error> {
        let raw_shared_ptr = deserializer
            .deserialize_shared::<T, rc::Rc<T>, _>(self.deref(), |ptr| {
                rc::Rc::<T>::from(unsafe { Box::from_raw(ptr) })
            })?;
        let shared_ptr = unsafe { rc::Rc::<T>::from_raw(raw_shared_ptr) };
        forget(shared_ptr.clone());
        Ok(shared_ptr)
    }
}

/// The resolver for `rc::Weak`.
pub enum RcWeakResolver<T> {
    /// The weak pointer was null
    None,
    /// The weak pointer was to some shared pointer
    Some(RcResolver<T>),
}

/// An archived `rc::Weak`.
#[repr(u8)]
pub enum ArchivedRcWeak<T: ArchivePointee + ?Sized> {
    /// A null weak pointer
    None,
    /// A weak pointer to some shared pointer
    Some(ArchivedRc<T>),
}

#[allow(dead_code)]
#[repr(u8)]
enum ArchivedRcWeakTag {
    None,
    Some,
}

#[repr(C)]
struct ArchivedRcWeakVariantNone(ArchivedRcWeakTag);

#[repr(C)]
struct ArchivedRcWeakVariantSome<T: ArchivePointee + ?Sized>(ArchivedRcWeakTag, ArchivedRc<T>);

impl<T: ArchivePointee + ?Sized> ArchivedRcWeak<T> {
    /// Attempts to upgrade the weak pointer to an `ArchivedArc`.
    ///
    /// Returns `None` if a null weak pointer was serialized.
    #[inline]
    pub fn upgrade(&self) -> Option<&ArchivedRc<T>> {
        match self {
            ArchivedRcWeak::None => None,
            ArchivedRcWeak::Some(r) => Some(r),
        }
    }

    /// Attempts to upgrade a pinned mutable weak pointer.
    #[inline]
    pub fn upgrade_pin(self: Pin<&mut Self>) -> Option<Pin<&mut ArchivedRc<T>>> {
        unsafe {
            match self.get_unchecked_mut() {
                ArchivedRcWeak::None => None,
                ArchivedRcWeak::Some(r) => Some(Pin::new_unchecked(r)),
            }
        }
    }
}

impl<T: ArchiveUnsized + ?Sized> Archive for rc::Weak<T> {
    type Archived = ArchivedRcWeak<T::Archived>;
    type Resolver = RcWeakResolver<T::MetadataResolver>;

    #[inline]
    unsafe fn resolve(&self, pos: usize, resolver: Self::Resolver, out: &mut MaybeUninit<Self::Archived>) {
        match resolver {
            RcWeakResolver::None => {
                let out = &mut *out
                    .as_mut_ptr()
                    .cast::<MaybeUninit<ArchivedRcWeakVariantNone>>();
                ptr::addr_of_mut!((*out.as_mut_ptr()).0).write(ArchivedRcWeakTag::None);
            },
            RcWeakResolver::Some(resolver) => {
                let out = &mut *out
                    .as_mut_ptr()
                    .cast::<MaybeUninit<ArchivedRcWeakVariantSome<T::Archived>>>();
                ptr::addr_of_mut!((*out.as_mut_ptr()).0).write(ArchivedRcWeakTag::Some);

                let (fp, fo) = out_field!(out.1);
                self.upgrade().unwrap().resolve(pos + fp, resolver, fo);
            },
        }
    }
}

impl<T: SerializeUnsized<S> + ?Sized + 'static, S: SharedSerializer + ?Sized> Serialize<S>
    for rc::Weak<T>
{
    #[inline]
    fn serialize(&self, serializer: &mut S) -> Result<Self::Resolver, S::Error> {
        Ok(match self.upgrade() {
            None => RcWeakResolver::None,
            Some(r) => RcWeakResolver::Some(r.serialize(serializer)?),
        })
    }
}

// Deserialize can only be implemented for sized types because weak pointers don't have from/into
// raw functions.
impl<T: Archive + 'static, D: SharedDeserializer + ?Sized> Deserialize<rc::Weak<T>, D>
    for Archived<rc::Weak<T>>
where
    T::Archived: DeserializeUnsized<T, D>,
{
    #[inline]
    fn deserialize(&self, deserializer: &mut D) -> Result<rc::Weak<T>, D::Error> {
        Ok(match self {
            ArchivedRcWeak::None => rc::Weak::new(),
            ArchivedRcWeak::Some(r) => rc::Rc::downgrade(&r.deserialize(deserializer)?),
        })
    }
}

impl<T: ?Sized> SharedPointer for sync::Arc<T> {
    #[inline]
    fn data_address(&self) -> *const () {
        sync::Arc::as_ptr(self) as *const ()
    }
}

/// The resolver for `Arc`.
pub struct ArcResolver<T> {
    pos: usize,
    metadata_resolver: T,
}

/// An archived `Arc`.
///
/// This is a thin wrapper around a [`RelPtr`] to the archived type.
#[repr(transparent)]
pub struct ArchivedArc<T: ArchivePointee + ?Sized>(RelPtr<T>);

impl<T: ArchivePointee + ?Sized> ArchivedArc<T> {
    /// Gets the value of this archived `Arc`.
    ///
    /// # Safety
    ///
    /// Any other `ArchivedArc` pointers to the same value must not be dereferenced for the duration
    /// of the returned borrow.
    #[inline]
    pub unsafe fn get_pin_unchecked(self: Pin<&mut Self>) -> Pin<&mut T> {
        self.map_unchecked_mut(|s| &mut *s.0.as_mut_ptr())
    }
}

impl<T: ArchivePointee + ?Sized> Deref for ArchivedArc<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0.as_ptr() }
    }
}

impl<T: ArchivePointee + PartialEq<U> + ?Sized, U: ?Sized> PartialEq<sync::Arc<U>>
    for ArchivedArc<T>
{
    #[inline]
    fn eq(&self, other: &sync::Arc<U>) -> bool {
        self.deref().eq(other.deref())
    }
}

impl<T: ArchiveUnsized + ?Sized> Archive for sync::Arc<T> {
    type Archived = ArchivedArc<T::Archived>;
    type Resolver = ArcResolver<T::MetadataResolver>;

    #[inline]
    unsafe fn resolve(&self, pos: usize, resolver: Self::Resolver, out: &mut MaybeUninit<Self::Archived>) {
        let (fp, fo) = out_field!(out.0);
        self.as_ref()
            .resolve_unsized(pos + fp, resolver.pos, resolver.metadata_resolver, fo);
    }
}

impl<T: SerializeUnsized<S> + ?Sized + 'static, S: SharedSerializer + ?Sized> Serialize<S>
    for sync::Arc<T>
{
    #[inline]
    fn serialize(&self, serializer: &mut S) -> Result<Self::Resolver, S::Error> {
        Ok(ArcResolver {
            pos: serializer.serialize_shared(self.deref())?,
            metadata_resolver: self.deref().serialize_metadata(serializer)?,
        })
    }
}

impl<T: ArchiveUnsized + ?Sized + 'static, D: SharedDeserializer + ?Sized>
    Deserialize<sync::Arc<T>, D> for Archived<sync::Arc<T>>
where
    T::Archived: DeserializeUnsized<T, D>,
{
    #[inline]
    fn deserialize(&self, deserializer: &mut D) -> Result<sync::Arc<T>, D::Error> {
        let raw_shared_ptr = deserializer.deserialize_shared(self.deref(), |ptr| {
            sync::Arc::<T>::from(unsafe { Box::from_raw(ptr) })
        })?;
        let shared_ptr = unsafe { sync::Arc::<T>::from_raw(raw_shared_ptr) };
        forget(shared_ptr.clone());
        Ok(shared_ptr)
    }
}

/// The resolver for `sync::Weak`.
pub enum ArcWeakResolver<T> {
    /// The weak pointer was null
    None,
    /// The weak pointer was to some shared pointer
    Some(ArcResolver<T>),
}

/// An archived `sync::Weak`.
#[repr(u8)]
pub enum ArchivedArcWeak<T: ArchivePointee + ?Sized> {
    /// A null weak pointer
    None,
    /// A weak pointer to some shared pointer
    Some(ArchivedArc<T>),
}

#[allow(dead_code)]
#[repr(u8)]
enum ArchivedArcWeakTag {
    None,
    Some,
}

#[repr(C)]
struct ArchivedArcWeakVariantNone(ArchivedArcWeakTag);

#[repr(C)]
struct ArchivedArcWeakVariantSome<T: ArchivePointee + ?Sized>(ArchivedArcWeakTag, ArchivedArc<T>);

impl<T: ArchivePointee + ?Sized> ArchivedArcWeak<T> {
    /// Attempts to upgrade the weak pointer to an `ArchivedArc`.
    ///
    /// Returns `None` if a null weak pointer was serialized.
    #[inline]
    pub fn upgrade(&self) -> Option<&ArchivedArc<T>> {
        match self {
            ArchivedArcWeak::None => None,
            ArchivedArcWeak::Some(r) => Some(r),
        }
    }

    /// Attempts to upgrade a pinned mutable weak pointer.
    #[inline]
    pub fn upgrade_pin(self: Pin<&mut Self>) -> Option<Pin<&mut ArchivedArc<T>>> {
        unsafe {
            match self.get_unchecked_mut() {
                ArchivedArcWeak::None => None,
                ArchivedArcWeak::Some(r) => Some(Pin::new_unchecked(r)),
            }
        }
    }
}

impl<T: ArchiveUnsized + ?Sized> Archive for sync::Weak<T> {
    type Archived = ArchivedArcWeak<T::Archived>;
    type Resolver = ArcWeakResolver<T::MetadataResolver>;

    #[inline]
    unsafe fn resolve(&self, pos: usize, resolver: Self::Resolver, out: &mut MaybeUninit<Self::Archived>) {
        match resolver {
            ArcWeakResolver::None => {
                let out = &mut *out
                    .as_mut_ptr()
                    .cast::<MaybeUninit<ArchivedArcWeakVariantNone>>();
                ptr::addr_of_mut!((*out.as_mut_ptr()).0).write(ArchivedArcWeakTag::None);
            },
            ArcWeakResolver::Some(resolver) => {
                let out = &mut *out
                    .as_mut_ptr()
                    .cast::<MaybeUninit<ArchivedArcWeakVariantSome<T::Archived>>>();
                ptr::addr_of_mut!((*out.as_mut_ptr()).0).write(ArchivedArcWeakTag::Some);

                let (fp, fo) = out_field!(out.1);
                self.upgrade().unwrap().resolve(pos + fp, resolver, fo);
            },
        }
    }
}

impl<T: SerializeUnsized<S> + ?Sized + 'static, S: SharedSerializer + ?Sized> Serialize<S>
    for sync::Weak<T>
{
    #[inline]
    fn serialize(&self, serializer: &mut S) -> Result<Self::Resolver, S::Error> {
        Ok(match self.upgrade() {
            None => ArcWeakResolver::None,
            Some(r) => ArcWeakResolver::Some(r.serialize(serializer)?),
        })
    }
}

// Deserialize can only be implemented for sized types because weak pointers don't have from/into
// raw functions.
impl<T: Archive + 'static, D: SharedDeserializer + ?Sized> Deserialize<sync::Weak<T>, D>
    for Archived<sync::Weak<T>>
where
    T::Archived: DeserializeUnsized<T, D>,
{
    #[inline]
    fn deserialize(&self, deserializer: &mut D) -> Result<sync::Weak<T>, D::Error> {
        Ok(match self {
            ArchivedArcWeak::None => sync::Weak::new(),
            ArchivedArcWeak::Some(r) => sync::Arc::downgrade(&r.deserialize(deserializer)?),
        })
    }
}
