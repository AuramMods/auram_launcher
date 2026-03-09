import 'dart:io';

import 'package:arcane/arcane.dart';
import 'package:archive/archive.dart';
import 'package:auram_launcher/pack/pack_java_utils.dart';
import 'package:auram_launcher/pack/pack_json_utils.dart';
import 'package:auram_launcher/pack/pack_library_utils.dart';
import 'package:auram_launcher/pack/pack_models.dart';
import 'package:auram_launcher/pack/pack_path_utils.dart';
import 'package:auram_launcher/pack/pack_runtime_service.dart';
import 'package:auram_launcher/pack/pack_types.dart';
import 'package:fast_log/fast_log.dart';
import 'package:microshaft/src/model/shafted.dart';

class PackLaunchService {
  const PackLaunchService._();

  static Future<void> launch({
    required Shafted auth,
    required BehaviorSubject<(String, double)?> progressStream,
    required Future<String> knownFlags,
    required Directory javaDir,
    required Directory gameDir,
    required Directory minecraftDir,
    required Directory librariesDir,
    required Directory versionsDir,
    required Directory assetsDir,
    required Directory nativesRootDir,
    required APlatform platform,
    required AArch arch,
  }) async {
    info("Preparing game launch");
    progressStream.add(("Preparing Launch", -1));
    (String, String) versions = await PackRuntimeService.readPackVersions(
      gameDir: gameDir,
    );
    String minecraftVersion = versions.$1;
    String forgeVersion = versions.$2;
    verbose("Launch versions: minecraft=$minecraftVersion forge=$forgeVersion");
    String forgeVersionId = await PackRuntimeService.resolveForgeVersionId(
      versionsDir: versionsDir,
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
    );
    verbose("Resolved forge version id for launch: $forgeVersionId");
    Map<String, dynamic> minecraftVersionJson =
        await PackRuntimeService.readInstalledVersionJson(
          versionsDir: versionsDir,
          versionId: minecraftVersion,
        );
    Map<String, dynamic> forgeVersionJson =
        await PackRuntimeService.readInstalledVersionJson(
          versionsDir: versionsDir,
          versionId: forgeVersionId,
        );
    List<dynamic> minecraftLibraries = PackJsonUtils.list(
      minecraftVersionJson["libraries"],
    );
    List<dynamic> forgeLibraries = PackJsonUtils.list(
      forgeVersionJson["libraries"],
    );
    verbose(
      "Launch library metadata counts: minecraft=${minecraftLibraries.length} forge=${forgeLibraries.length}",
    );
    Directory nativesDirectory = await _prepareNativesDirectory(
      nativesRootDir: nativesRootDir,
      librariesDir: librariesDir,
      progressStream: progressStream,
      platform: platform,
      arch: arch,
      minecraftLibraries: minecraftLibraries,
      forgeLibraries: forgeLibraries,
    );
    List<String> launchArguments = await _buildLaunchArguments(
      auth: auth,
      knownFlags: knownFlags,
      minecraftDir: minecraftDir,
      librariesDir: librariesDir,
      versionsDir: versionsDir,
      assetsDir: assetsDir,
      platform: platform,
      arch: arch,
      minecraftVersion: minecraftVersion,
      forgeVersionId: forgeVersionId,
      minecraftVersionJson: minecraftVersionJson,
      forgeVersionJson: forgeVersionJson,
      nativesDirectory: nativesDirectory,
      minecraftLibraries: minecraftLibraries,
      forgeLibraries: forgeLibraries,
    );
    info("Launch arguments built (${launchArguments.length} entries)");
    String javaExecutable = await PackJavaUtils.resolveJavaExecutable(
      javaDir: javaDir,
    );
    await PackJavaUtils.ensureExecutable(binaryPath: javaExecutable);
    Directory workingDirectory = minecraftDir;
    if (!await workingDirectory.exists()) {
      await workingDirectory.create(recursive: true);
    }

    progressStream.add(("Starting Game", -1));
    await _startDetachedGameProcess(
      javaExecutable: javaExecutable,
      launchArguments: launchArguments,
      workingDirectory: workingDirectory.path,
    );
    progressStream.add(("Game Started", 1));
    success("Game process started");
  }

  static Map<String, bool> _defaultLaunchFeatures() => <String, bool>{
    "is_demo_user": false,
    "has_custom_resolution": false,
    "has_quick_plays_support": false,
    "is_quick_play_singleplayer": false,
    "is_quick_play_multiplayer": false,
    "is_quick_play_realms": false,
  };

  static bool _matchesArgumentRule({
    required Map<String, dynamic> rule,
    required Map<String, bool> features,
    required APlatform platform,
    required AArch arch,
  }) {
    if (!PackLibraryUtils.matchesRule(
      rule: rule,
      platform: platform,
      arch: arch,
    )) {
      return false;
    }

    Map<String, dynamic> featureRules = PackJsonUtils.map(rule["features"]);
    if (featureRules.isEmpty) return true;

    for (MapEntry<String, dynamic> entry in featureRules.entries) {
      bool expected = entry.value == true;
      bool actual = features[entry.key] ?? false;
      if (actual != expected) return false;
    }

    return true;
  }

  static bool _isArgumentEntryAllowed({
    required Map<String, dynamic> entry,
    required Map<String, bool> features,
    required APlatform platform,
    required AArch arch,
  }) {
    List<dynamic> rules = PackJsonUtils.list(entry["rules"]);
    if (rules.isEmpty) return true;

    bool allowed = false;
    for (dynamic dynamicRule in rules) {
      Map<String, dynamic> rule = PackJsonUtils.map(dynamicRule);
      if (rule.isEmpty) continue;
      if (!_matchesArgumentRule(
        rule: rule,
        features: features,
        platform: platform,
        arch: arch,
      )) {
        continue;
      }
      allowed = rule["action"]?.toString() == "allow";
    }

    return allowed;
  }

  static void _appendArgumentValue({
    required List<String> output,
    required dynamic value,
  }) {
    if (value == null) return;
    if (value is List) {
      for (dynamic item in value) {
        if (item == null) continue;
        output.add(item.toString());
      }
      return;
    }
    output.add(value.toString());
  }

  static void _appendArgumentEntry({
    required List<String> output,
    required dynamic entry,
    required Map<String, bool> features,
    required APlatform platform,
    required AArch arch,
  }) {
    if (entry == null) return;
    if (entry is String) {
      output.add(entry);
      return;
    }

    Map<String, dynamic> entryMap = PackJsonUtils.map(entry);
    if (entryMap.isEmpty) return;
    if (!_isArgumentEntryAllowed(
      entry: entryMap,
      features: features,
      platform: platform,
      arch: arch,
    )) {
      return;
    }
    _appendArgumentValue(output: output, value: entryMap["value"]);
  }

  static List<String> _collectVersionArguments({
    required Map<String, dynamic> versionJson,
    required String side,
    required Map<String, bool> features,
    required APlatform platform,
    required AArch arch,
  }) {
    Map<String, dynamic> arguments = PackJsonUtils.map(
      versionJson["arguments"],
    );
    dynamic sideArguments = arguments[side];
    if (sideArguments is! List) return <String>[];

    List<String> output = <String>[];
    for (dynamic entry in sideArguments) {
      _appendArgumentEntry(
        output: output,
        entry: entry,
        features: features,
        platform: platform,
        arch: arch,
      );
    }

    return output;
  }

  static List<String> _splitArguments(String raw) {
    if (raw.trim().isEmpty) return <String>[];
    List<String> output = <String>[];
    List<String> pieces = raw.trim().split(RegExp(r"\s+"));
    for (String piece in pieces) {
      String trimmed = piece.trim();
      if (trimmed.isEmpty) continue;
      output.add(trimmed);
    }
    return output;
  }

  static List<String> _collectLegacyGameArguments(
    Map<String, dynamic> versionJson,
  ) {
    String raw = versionJson["minecraftArguments"]?.toString() ?? "";
    return _splitArguments(raw);
  }

  static List<String> _mergeArguments({
    required List<String> base,
    required List<String> child,
    required bool inheritsFromBase,
  }) {
    if (inheritsFromBase) {
      List<String> merged = <String>[];
      merged.addAll(base);
      merged.addAll(child);
      return merged;
    }

    if (child.isNotEmpty) return child;
    return base;
  }

  static String _applyPlaceholders({
    required String input,
    required Map<String, String> values,
  }) {
    String output = input;
    for (MapEntry<String, String> entry in values.entries) {
      String needle = "\${${entry.key}}";
      output = output.replaceAll(needle, entry.value);
    }
    return output;
  }

  static List<String> _applyPlaceholderList({
    required List<String> input,
    required Map<String, String> values,
  }) {
    List<String> output = <String>[];
    for (String item in input) {
      output.add(_applyPlaceholders(input: item, values: values));
    }
    return output;
  }

  static void _appendIgnoreListJar({
    required List<String> jvmArgs,
    required String jarName,
  }) {
    if (jarName.isEmpty) return;
    String key = "-DignoreList=";
    for (int i = 0; i < jvmArgs.length; i++) {
      String arg = jvmArgs[i];
      if (!arg.startsWith(key)) continue;
      String existing = arg.substring(key.length);
      List<String> entries = existing.split(",");
      for (String entry in entries) {
        if (entry.trim() == jarName) return;
      }
      jvmArgs[i] = "$key$existing,$jarName";
      return;
    }
  }

  static bool _hasClasspathArgument(List<String> jvmArgs) {
    for (String arg in jvmArgs) {
      if (arg == "-cp" || arg == "-classpath") return true;
    }
    return false;
  }

  static void _addUniquePath({
    required List<String> output,
    required Set<String> seen,
    required String path,
  }) {
    if (!seen.contains(path)) {
      seen.add(path);
      output.add(path);
    }
  }

  static Future<List<String>> _collectClasspathEntries({
    required Directory librariesDir,
    required Directory versionsDir,
    required APlatform platform,
    required AArch arch,
    required String minecraftVersion,
    required String forgeVersionId,
    required Map<String, dynamic> forgeVersionJson,
    required List<dynamic> minecraftLibraries,
    required List<dynamic> forgeLibraries,
  }) async {
    verbose("Collecting classpath entries");
    Map<String, DownloadTarget> downloadMap = <String, DownloadTarget>{};
    for (dynamic dynamicLibrary in minecraftLibraries) {
      Map<String, dynamic> library = PackJsonUtils.map(dynamicLibrary);
      if (library.isEmpty) continue;
      PackLibraryUtils.addLibraryDownloads(
        library: library,
        outputs: downloadMap,
        librariesDir: librariesDir,
        platform: platform,
        arch: arch,
      );
    }
    for (dynamic dynamicLibrary in forgeLibraries) {
      Map<String, dynamic> library = PackJsonUtils.map(dynamicLibrary);
      if (library.isEmpty) continue;
      PackLibraryUtils.addLibraryDownloads(
        library: library,
        outputs: downloadMap,
        librariesDir: librariesDir,
        platform: platform,
        arch: arch,
      );
    }

    Set<String> seen = <String>{};
    List<String> classpath = <String>[];
    for (DownloadTarget download in downloadMap.values) {
      if (!await download.file.exists()) continue;
      _addUniquePath(output: classpath, seen: seen, path: download.file.path);
    }

    String inheritedJarVersion = forgeVersionJson["jar"]?.toString() ?? "";
    if (inheritedJarVersion.isEmpty) inheritedJarVersion = minecraftVersion;
    File inheritedJar = PackRuntimeService.versionJarFile(
      versionsDir: versionsDir,
      versionId: inheritedJarVersion,
    );
    if (await inheritedJar.exists()) {
      _addUniquePath(output: classpath, seen: seen, path: inheritedJar.path);
    }

    File minecraftJar = PackRuntimeService.versionJarFile(
      versionsDir: versionsDir,
      versionId: minecraftVersion,
    );
    if (await minecraftJar.exists()) {
      _addUniquePath(output: classpath, seen: seen, path: minecraftJar.path);
    }

    File forgeJar = PackRuntimeService.versionJarFile(
      versionsDir: versionsDir,
      versionId: forgeVersionId,
    );
    if (await forgeJar.exists()) {
      _addUniquePath(output: classpath, seen: seen, path: forgeJar.path);
    }

    if (classpath.isEmpty) {
      throw Exception("Classpath is empty after resolving libraries and jars");
    }

    info("Classpath entries resolved: ${classpath.length}");
    return classpath;
  }

  static Future<List<File>> _collectNativeJarFiles({
    required Directory librariesDir,
    required APlatform platform,
    required AArch arch,
    required List<dynamic> minecraftLibraries,
    required List<dynamic> forgeLibraries,
  }) async {
    verbose("Collecting native library jars");
    List<File> files = <File>[];
    Set<String> seen = <String>{};
    List<dynamic> combined = <dynamic>[];
    combined.addAll(minecraftLibraries);
    combined.addAll(forgeLibraries);

    for (dynamic dynamicLibrary in combined) {
      Map<String, dynamic> library = PackJsonUtils.map(dynamicLibrary);
      if (library.isEmpty) continue;
      if (!PackLibraryUtils.isAllowedByRules(
        entry: library,
        platform: platform,
        arch: arch,
      )) {
        continue;
      }
      String? classifier = PackLibraryUtils.resolveNativeClassifier(
        library: library,
        platform: platform,
        arch: arch,
      );
      if (classifier == null || classifier.isEmpty) continue;

      Map<String, dynamic> downloads = PackJsonUtils.map(library["downloads"]);
      Map<String, dynamic> classifiers = PackJsonUtils.map(
        downloads["classifiers"],
      );
      Map<String, dynamic> nativeArtifact = PackJsonUtils.map(
        classifiers[classifier],
      );
      String path = nativeArtifact["path"]?.toString() ?? "";
      if (path.isEmpty) {
        String name = library["name"]?.toString() ?? "";
        if (name.isNotEmpty) {
          path = PackLibraryUtils.mavenPathFromName("$name:$classifier") ?? "";
        }
      }
      if (path.isEmpty) continue;

      String localPath =
          "${librariesDir.path}${Platform.pathSeparator}${PackPathUtils.toPlatformPath(path)}";
      if (seen.contains(localPath)) continue;
      seen.add(localPath);
      File nativeJar = File(localPath);
      if (await nativeJar.exists()) {
        files.add(nativeJar);
      }
    }

    info("Native jars resolved: ${files.length}");
    return files;
  }

  static Future<void> _extractNativeJar({
    required File jarFile,
    required Directory destination,
  }) async {
    if (!await jarFile.exists()) return;
    List<int> bytes = await jarFile.readAsBytes();
    Archive archive = ZipDecoder().decodeBytes(bytes);
    for (ArchiveFile entry in archive) {
      if (!entry.isFile) continue;
      if (entry.name.startsWith("META-INF/")) continue;
      List<int>? entryBytes = entry.readBytes();
      if (entryBytes == null) continue;
      String localName = PackPathUtils.toPlatformPath(entry.name);
      File out = File("${destination.path}${Platform.pathSeparator}$localName");
      Directory parent = out.parent;
      if (!await parent.exists()) {
        await parent.create(recursive: true);
      }
      await out.writeAsBytes(entryBytes);
    }
  }

  static Future<Directory> _prepareNativesDirectory({
    required Directory nativesRootDir,
    required Directory librariesDir,
    required BehaviorSubject<(String, double)?> progressStream,
    required APlatform platform,
    required AArch arch,
    required List<dynamic> minecraftLibraries,
    required List<dynamic> forgeLibraries,
  }) async {
    info("Preparing natives directory");
    Directory root = nativesRootDir;
    if (!await root.exists()) {
      await root.create(recursive: true);
    }

    String launchDirName = DateTime.now().millisecondsSinceEpoch.toString();
    Directory launchNatives = Directory(
      "${root.path}${Platform.pathSeparator}$launchDirName",
    );
    if (await launchNatives.exists()) {
      await launchNatives.delete(recursive: true);
    }
    await launchNatives.create(recursive: true);
    verbose("Launch natives directory: ${launchNatives.path}");

    List<File> nativeJars = await _collectNativeJarFiles(
      librariesDir: librariesDir,
      platform: platform,
      arch: arch,
      minecraftLibraries: minecraftLibraries,
      forgeLibraries: forgeLibraries,
    );
    int total = nativeJars.length;
    verbose("Native jar extraction targets: $total");
    int index = 0;
    for (File nativeJar in nativeJars) {
      index += 1;
      progressStream.add(("Extracting Natives ($index/$total)", index / total));
      await _extractNativeJar(jarFile: nativeJar, destination: launchNatives);
    }

    success("Natives prepared in ${launchNatives.path}");
    return launchNatives;
  }

  static Future<List<String>> _collectJvmFlagArguments({
    required Future<String> knownFlags,
  }) async => _splitArguments(await knownFlags);

  static Future<List<String>> _buildLaunchArguments({
    required Shafted auth,
    required Future<String> knownFlags,
    required Directory minecraftDir,
    required Directory librariesDir,
    required Directory versionsDir,
    required Directory assetsDir,
    required APlatform platform,
    required AArch arch,
    required String minecraftVersion,
    required String forgeVersionId,
    required Map<String, dynamic> minecraftVersionJson,
    required Map<String, dynamic> forgeVersionJson,
    required Directory nativesDirectory,
    required List<dynamic> minecraftLibraries,
    required List<dynamic> forgeLibraries,
  }) async {
    info("Building launch arguments");
    Map<String, bool> features = _defaultLaunchFeatures();
    bool inheritsFromBase =
        (forgeVersionJson["inheritsFrom"]?.toString() ?? "").isNotEmpty;

    List<String> minecraftJvmArgs = _collectVersionArguments(
      versionJson: minecraftVersionJson,
      side: "jvm",
      features: features,
      platform: platform,
      arch: arch,
    );
    List<String> forgeJvmArgs = _collectVersionArguments(
      versionJson: forgeVersionJson,
      side: "jvm",
      features: features,
      platform: platform,
      arch: arch,
    );
    List<String> minecraftGameArgs = _collectVersionArguments(
      versionJson: minecraftVersionJson,
      side: "game",
      features: features,
      platform: platform,
      arch: arch,
    );
    List<String> forgeGameArgs = _collectVersionArguments(
      versionJson: forgeVersionJson,
      side: "game",
      features: features,
      platform: platform,
      arch: arch,
    );

    List<String> mergedJvmArgs = _mergeArguments(
      base: minecraftJvmArgs,
      child: forgeJvmArgs,
      inheritsFromBase: inheritsFromBase,
    );
    List<String> mergedGameArgs = _mergeArguments(
      base: minecraftGameArgs,
      child: forgeGameArgs,
      inheritsFromBase: inheritsFromBase,
    );
    if (mergedGameArgs.isEmpty) {
      List<String> forgeLegacy = _collectLegacyGameArguments(forgeVersionJson);
      List<String> minecraftLegacy = _collectLegacyGameArguments(
        minecraftVersionJson,
      );
      mergedGameArgs = _mergeArguments(
        base: minecraftLegacy,
        child: forgeLegacy,
        inheritsFromBase: inheritsFromBase,
      );
    }

    List<String> classpathEntries = await _collectClasspathEntries(
      librariesDir: librariesDir,
      versionsDir: versionsDir,
      platform: platform,
      arch: arch,
      minecraftVersion: minecraftVersion,
      forgeVersionId: forgeVersionId,
      forgeVersionJson: forgeVersionJson,
      minecraftLibraries: minecraftLibraries,
      forgeLibraries: forgeLibraries,
    );
    String inheritedJarVersion = forgeVersionJson["jar"]?.toString() ?? "";
    if (inheritedJarVersion.isEmpty) {
      inheritedJarVersion = minecraftVersion;
    }

    String classpathSeparator = Platform.isWindows ? ";" : ":";
    String classpath = classpathEntries.join(classpathSeparator);
    String authPlayerName = auth.profileName?.toString() ?? "";
    if (authPlayerName.isEmpty) {
      authPlayerName = auth.username?.toString() ?? "Player";
    }
    String authUuid = auth.uuid?.toString() ?? "";
    if (authUuid.isEmpty) {
      authUuid = "00000000000000000000000000000000";
    }
    String authToken = auth.mojangToken?.toString() ?? "";
    if (authToken.isEmpty) {
      authToken = auth.microsoftAccessToken?.toString() ?? "";
    }
    if (authToken.isEmpty) {
      authToken = "0";
    }

    Map<String, dynamic> assetIndex = PackJsonUtils.map(
      minecraftVersionJson["assetIndex"],
    );
    String assetIndexName = assetIndex["id"]?.toString() ?? "";
    String versionType = forgeVersionJson["type"]?.toString() ?? "";
    if (versionType.isEmpty) {
      versionType = minecraftVersionJson["type"]?.toString() ?? "release";
    }

    Map<String, String> placeholders = <String, String>{
      "auth_player_name": authPlayerName,
      "version_name": forgeVersionId,
      "game_directory": minecraftDir.path,
      "assets_root": assetsDir.path,
      "game_assets": assetsDir.path,
      "assets_index_name": assetIndexName,
      "auth_uuid": authUuid,
      "auth_access_token": authToken,
      "auth_session": "token:$authToken:$authUuid",
      "clientid": "",
      "xuid": auth.userHash?.toString() ?? "",
      "auth_xuid": auth.userHash?.toString() ?? "",
      "user_type": "msa",
      "version_type": versionType,
      "natives_directory": nativesDirectory.path,
      "launcher_name": "AuramLauncher",
      "launcher_version": "1.0.0",
      "classpath": classpath,
      "classpath_separator": classpathSeparator,
      "library_directory": librariesDir.path,
      "user_properties": "{}",
    };

    List<String> resolvedJvmArgs = _applyPlaceholderList(
      input: mergedJvmArgs,
      values: placeholders,
    );
    List<String> resolvedGameArgs = _applyPlaceholderList(
      input: mergedGameArgs,
      values: placeholders,
    );
    _appendIgnoreListJar(
      jvmArgs: resolvedJvmArgs,
      jarName: "$inheritedJarVersion.jar",
    );

    if (!_hasClasspathArgument(resolvedJvmArgs)) {
      resolvedJvmArgs.add("-cp");
      resolvedJvmArgs.add(classpath);
    }

    List<String> allJvmArgs = <String>[];
    allJvmArgs.addAll(resolvedJvmArgs);
    allJvmArgs.addAll(await _collectJvmFlagArguments(knownFlags: knownFlags));
    verbose(
      "Launch argument sections: jvm=${allJvmArgs.length} game=${resolvedGameArgs.length}",
    );

    String mainClass = forgeVersionJson["mainClass"]?.toString() ?? "";
    if (mainClass.isEmpty) {
      mainClass = minecraftVersionJson["mainClass"]?.toString() ?? "";
    }
    if (mainClass.isEmpty) {
      throw Exception("Missing mainClass in installed version metadata");
    }

    List<String> launchArguments = <String>[];
    launchArguments.addAll(allJvmArgs);
    launchArguments.add(mainClass);
    launchArguments.addAll(resolvedGameArgs);
    success("Launch argument build complete");
    return launchArguments;
  }

  static Future<void> _startDetachedGameProcess({
    required String javaExecutable,
    required List<String> launchArguments,
    required String workingDirectory,
  }) async {
    info("Starting detached game process in $workingDirectory");
    verbose("Java executable: $javaExecutable");
    verbose("Launch argument count: ${launchArguments.length}");
    await Process.start(
      javaExecutable,
      launchArguments,
      workingDirectory: workingDirectory,
      mode: ProcessStartMode.detached,
    );
    success("Detached process spawned");
  }
}
