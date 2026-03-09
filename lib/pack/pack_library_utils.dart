import 'dart:io';

import 'package:auram_launcher/pack/pack_constants.dart';
import 'package:auram_launcher/pack/pack_json_utils.dart';
import 'package:auram_launcher/pack/pack_models.dart';
import 'package:auram_launcher/pack/pack_path_utils.dart';
import 'package:auram_launcher/pack/pack_types.dart';

class PackLibraryUtils {
  const PackLibraryUtils._();

  static String currentOsName({required APlatform platform}) =>
      switch (platform) {
        APlatform.macos => "osx",
        APlatform.windows => "windows",
        APlatform.linux => "linux",
      };

  static String ruleArch({required AArch arch}) => switch (arch) {
    AArch.x64 => "x86_64",
    AArch.arm64 => "aarch64",
  };

  static String nativeArchToken({required AArch arch}) => switch (arch) {
    AArch.x64 => "64",
    AArch.arm64 => "arm64",
  };

  static bool matchesRule({
    required Map<String, dynamic> rule,
    required APlatform platform,
    required AArch arch,
  }) {
    Map<String, dynamic> os = PackJsonUtils.map(rule["os"]);
    if (os.isEmpty) return true;

    String name = os["name"]?.toString() ?? "";
    if (name.isNotEmpty && name != currentOsName(platform: platform)) {
      return false;
    }

    String archPattern = os["arch"]?.toString() ?? "";
    if (archPattern.isNotEmpty &&
        !RegExp(archPattern).hasMatch(ruleArch(arch: arch))) {
      return false;
    }

    return true;
  }

  static bool isAllowedByRules({
    required Map<String, dynamic> entry,
    required APlatform platform,
    required AArch arch,
  }) {
    List<dynamic> rules = PackJsonUtils.list(entry["rules"]);
    if (rules.isEmpty) return true;

    bool allowed = false;
    for (dynamic dynamicRule in rules) {
      Map<String, dynamic> rule = PackJsonUtils.map(dynamicRule);
      if (rule.isEmpty) continue;
      if (!matchesRule(rule: rule, platform: platform, arch: arch)) continue;
      allowed = rule["action"]?.toString() == "allow";
    }
    return allowed;
  }

  static String? resolveNativeClassifier({
    required Map<String, dynamic> library,
    required APlatform platform,
    required AArch arch,
  }) {
    Map<String, dynamic> natives = PackJsonUtils.map(library["natives"]);
    if (natives.isEmpty) return null;

    String classifier =
        natives[currentOsName(platform: platform)]?.toString() ?? "";
    if (classifier.isEmpty) return null;
    return classifier.replaceAll(r'${arch}', nativeArchToken(arch: arch));
  }

  static String? mavenPathFromName(String coordinate) {
    List<String> parts = coordinate.split(":");
    if (parts.length < 3) return null;

    String group = parts[0].replaceAll(".", "/");
    String artifact = parts[1];
    String version = parts[2];
    String classifierAndExt = parts.length > 3 ? parts[3] : "";
    String classifier = "";
    String extension = "jar";

    if (classifierAndExt.isNotEmpty) {
      List<String> classifierParts = classifierAndExt.split("@");
      classifier = classifierParts[0];
      if (classifierParts.length > 1 && classifierParts[1].isNotEmpty) {
        extension = classifierParts[1];
      }
    }

    String fileName = "$artifact-$version";
    if (classifier.isNotEmpty) {
      fileName = "$fileName-$classifier";
    }
    fileName = "$fileName.$extension";
    return "$group/$artifact/$version/$fileName";
  }

  static Uri libraryBaseUri(Map<String, dynamic> library) {
    String base = library["url"]?.toString() ?? "";
    if (base.isEmpty) base = PackConstants.mojangLibraryBaseUrl;
    if (!base.endsWith("/")) base = "$base/";
    return Uri.parse(base);
  }

  static void addLibraryDownloads({
    required Map<String, dynamic> library,
    required Map<String, DownloadTarget> outputs,
    required Directory librariesDir,
    required APlatform platform,
    required AArch arch,
  }) {
    if (!isAllowedByRules(entry: library, platform: platform, arch: arch)) {
      return;
    }

    Map<String, dynamic> downloads = PackJsonUtils.map(library["downloads"]);
    if (downloads.isNotEmpty) {
      Map<String, dynamic> artifact = PackJsonUtils.map(downloads["artifact"]);
      if (artifact.isNotEmpty) {
        String path = artifact["path"]?.toString() ?? "";
        String url = artifact["url"]?.toString() ?? "";
        int size = artifact["size"] is num
            ? (artifact["size"] as num).toInt()
            : 0;
        if (path.isNotEmpty && url.isNotEmpty) {
          outputs[path] = DownloadTarget(
            uri: Uri.parse(url),
            file: File(
              "${librariesDir.path}${Platform.pathSeparator}${PackPathUtils.toPlatformPath(path)}",
            ),
            size: size,
          );
        }
      } else {
        String name = library["name"]?.toString() ?? "";
        String? path = mavenPathFromName(name);
        if (path != null) {
          Uri base = libraryBaseUri(library);
          outputs[path] = DownloadTarget(
            uri: base.resolve(path),
            file: File(
              "${librariesDir.path}${Platform.pathSeparator}${PackPathUtils.toPlatformPath(path)}",
            ),
            size: 0,
          );
        }
      }

      String? nativeClassifier = resolveNativeClassifier(
        library: library,
        platform: platform,
        arch: arch,
      );
      Map<String, dynamic> classifiers = PackJsonUtils.map(
        downloads["classifiers"],
      );
      Map<String, dynamic> nativeArtifact = PackJsonUtils.map(
        nativeClassifier == null ? null : classifiers[nativeClassifier],
      );
      if (nativeArtifact.isNotEmpty) {
        String path = nativeArtifact["path"]?.toString() ?? "";
        String url = nativeArtifact["url"]?.toString() ?? "";
        int size = nativeArtifact["size"] is num
            ? (nativeArtifact["size"] as num).toInt()
            : 0;
        if (path.isNotEmpty && url.isNotEmpty) {
          outputs[path] = DownloadTarget(
            uri: Uri.parse(url),
            file: File(
              "${librariesDir.path}${Platform.pathSeparator}${PackPathUtils.toPlatformPath(path)}",
            ),
            size: size,
          );
        }
      }

      return;
    }

    String name = library["name"]?.toString() ?? "";
    String? path = mavenPathFromName(name);
    if (path == null) return;

    Uri base = libraryBaseUri(library);
    outputs[path] = DownloadTarget(
      uri: base.resolve(path),
      file: File(
        "${librariesDir.path}${Platform.pathSeparator}${PackPathUtils.toPlatformPath(path)}",
      ),
      size: 0,
    );
  }
}
