import 'dart:io';

class PackPathUtils {
  const PackPathUtils._();

  static String basename(String path) {
    List<String> parts = path
        .split(RegExp(r"[\\/]+"))
        .where((v) => v.isNotEmpty)
        .toList();
    if (parts.isEmpty) return path;
    return parts.last;
  }

  static String joinPath(List<String> parts) =>
      parts.join(Platform.pathSeparator);

  static String toPlatformPath(String path) =>
      path.replaceAll("/", Platform.pathSeparator);
}
