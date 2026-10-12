/// Local builds fall back to pubspec; Release builds embed the Git tag version.
const rustfrpAppVersion = String.fromEnvironment(
  'RUSTFRP_VERSION',
  defaultValue: '0.1.0',
);
