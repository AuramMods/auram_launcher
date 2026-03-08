import 'package:arcane/arcane.dart';

class PackDataUtils {
  const PackDataUtils._();

  static String? getString(String k) => hotBox.get(k);
  static void putString(String k, String v) => hotBox.put(k, v);

  static String? getCurrentVersion() => getString("currentVersion");
  static void setCurrentVersion(String version) =>
      putString("currentVersion", version);
}
