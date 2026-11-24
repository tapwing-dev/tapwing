/// In-app driver for Tapwing.
///
/// Black-box contract: the driver only reads what the Flutter engine would
/// send to the platform accessibility layer (the semantics tree) and only
/// injects what the engine would deliver from the OS (pointer packets,
/// text input and navigation platform messages). It never touches widget
/// state or calls callbacks directly.
library;

export 'src/binding.dart' show TapwingBinding, TapwingBindingMixin;
