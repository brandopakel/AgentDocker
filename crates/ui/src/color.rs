//! Exact byte colours for persisted terminal palettes and ANSI conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(u8, u8, u8);
impl Rgb {
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self(r, g, b)
    }
    pub const fn r(self) -> u8 {
        self.0
    }
    pub const fn g(self) -> u8 {
        self.1
    }
    pub const fn b(self) -> u8 {
        self.2
    }
}
impl From<Rgb> for iced::Color {
    fn from(value: Rgb) -> Self {
        Self::from_rgb8(value.0, value.1, value.2)
    }
}
