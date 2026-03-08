import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:arcane/arcane.dart';
import 'package:archive/archive.dart';
import 'package:auram_launcher/pack/pack_constants.dart';
import 'package:auram_launcher/pack/pack_data_utils.dart';
import 'package:auram_launcher/pack/pack_file_utils.dart';
import 'package:auram_launcher/pack/pack_json_utils.dart';
import 'package:auram_launcher/pack/pack_path_utils.dart';
import 'package:auram_launcher/pack/pack_platform_utils.dart';
import 'package:auram_launcher/pack/pack_types.dart';
import 'package:fast_log/fast_log.dart';
import 'package:microshaft/src/model/shafted.dart';
import 'package:path_provider/path_provider.dart';

class PackInstance {
  final BehaviorSubject<(String, double)?> progressStream;

  late Future<String> knownFlags;
  late Directory launcherDir;
  late Directory javaDir;
  late Directory gameDir;
  late Directory tempDir;
  Process? gameProcess;
  StreamSubscription<String>? gameStdoutSubscription;
  StreamSubscription<String>? gameStderrSubscription;
  IOSink? gameLogSink;

  PackInstance()
    : progressStream = BehaviorSubject.seeded(null),
      knownFlags = PackConstants.jvmFlags;

  Future<void> launch(Shafted auth) async {
    progressStream.add(("Preparing Launch", -1));
    (String, String) versions = await _readPackVersions();
    String minecraftVersion = versions.$1;
    String forgeVersion = versions.$2;
    String forgeVersionId = await _resolveForgeVersionId(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
    );
    Map<String, dynamic> minecraftVersionJson = await _readInstalledVersionJson(
      minecraftVersion,
    );
    Map<String, dynamic> forgeVersionJson = await _readInstalledVersionJson(
      forgeVersionId,
    );
    List<dynamic> minecraftLibraries = PackJsonUtils.list(
      minecraftVersionJson["libraries"],
    );
    List<dynamic> forgeLibraries = PackJsonUtils.list(
      forgeVersionJson["libraries"],
    );
    Directory nativesDirectory = await _prepareNativesDirectory(
      minecraftLibraries: minecraftLibraries,
      forgeLibraries: forgeLibraries,
    );
    List<String> launchArguments = await _buildLaunchArguments(
      auth: auth,
      minecraftVersion: minecraftVersion,
      forgeVersionId: forgeVersionId,
      minecraftVersionJson: minecraftVersionJson,
      forgeVersionJson: forgeVersionJson,
      nativesDirectory: nativesDirectory,
      minecraftLibraries: minecraftLibraries,
      forgeLibraries: forgeLibraries,
    );
    String javaExecutable = await _resolveJavaExecutable();
    await _ensureExecutable(javaExecutable);
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
  }

  APlatform get currentPlatform => PackPlatformUtils.currentPlatform();

  AArch get currentArch => PackPlatformUtils.currentArch();

  String get jdkDownload => PackPlatformUtils.jdkDownload();

  Directory get minecraftDir =>
      Directory("${gameDir.path}${Platform.pathSeparator}minecraft");

  Directory get librariesDir =>
      Directory("${minecraftDir.path}${Platform.pathSeparator}libraries");

  Directory get versionsDir =>
      Directory("${minecraftDir.path}${Platform.pathSeparator}versions");

  Directory get assetsDir =>
      Directory("${minecraftDir.path}${Platform.pathSeparator}assets");

  Directory get nativesRootDir =>
      Directory("${minecraftDir.path}${Platform.pathSeparator}natives");

  Future<void> initialize() => getApplicationSupportDirectory()
      .then(
        (v) => Directory(
          PackPathUtils.joinPath(<String>[v.absolute.path, "Auram"]),
        ),
      )
      .then((v) async {
        await v.create(recursive: true);
        launcherDir = v;
        tempDir = Directory(
          PackPathUtils.joinPath(<String>[v.absolute.path, "temp"]),
        );
        if (await tempDir.exists()) await tempDir.delete(recursive: true);
        await tempDir.create(recursive: true);
        javaDir = Directory(
          PackPathUtils.joinPath(<String>[v.absolute.path, "jvm"]),
        );
        gameDir = Directory(
          PackPathUtils.joinPath(<String>[v.absolute.path, "minecraft"]),
        );
        verbose("Launcher: ${launcherDir.absolute.path}");
        await ensureInstall();
        progressStream.add(null);
      });

  void dispose() {
    _resetGameOutputTracking();
    progressStream.close();
  }

  Future<void> _downloadFile({
    required Uri uri,
    required File target,
    required String progressLabel,
    required String itemName,
    int? expectedTotalBytes,
    int? assumedTotalBytes,
    bool emitByteProgress = true,
  }) async {
    if (await target.exists()) {
      if (expectedTotalBytes == null || expectedTotalBytes <= 0) return;
      int existingLength = await target.length();
      if (existingLength == expectedTotalBytes) return;
      await target.delete();
    }

    Directory parent = target.parent;
    if (!await parent.exists()) {
      await parent.create(recursive: true);
    }

    HttpClient client = HttpClient();
    client.connectionTimeout = Duration(seconds: 30);
    try {
      HttpClientRequest request = await client.getUrl(uri);
      HttpClientResponse response = await request.close();
      if (response.statusCode != HttpStatus.ok) {
        throw HttpException(
          "Failed to download $itemName (status ${response.statusCode})",
          uri: uri,
        );
      }

      IOSink sink = target.openWrite();
      int downloaded = 0;
      int totalBytes = response.contentLength > 0
          ? response.contentLength
          : (expectedTotalBytes != null && expectedTotalBytes > 0
                ? expectedTotalBytes
                : (assumedTotalBytes ?? -1));
      if (emitByteProgress) {
        if (totalBytes > 0) {
          progressStream.add((progressLabel, 0));
        } else {
          progressStream.add((progressLabel, -1));
        }
      }
      try {
        await for (List<int> chunk in response) {
          sink.add(chunk);
          downloaded += chunk.length;
          if (emitByteProgress && totalBytes > 0) {
            double progress = downloaded / totalBytes;
            if (progress > 1) progress = 1;
            progressStream.add((progressLabel, progress));
          }
        }
        if (emitByteProgress) {
          progressStream.add((progressLabel, 1));
        }
      } finally {
        await sink.close();
      }
    } finally {
      client.close(force: true);
    }
  }

  Future<Map<String, dynamic>> _readJsonFromUri(Uri uri) async {
    HttpClient client = HttpClient();
    client.connectionTimeout = Duration(seconds: 30);
    try {
      HttpClientRequest request = await client.getUrl(uri);
      HttpClientResponse response = await request.close();
      if (response.statusCode != HttpStatus.ok) {
        throw HttpException(
          "Failed to download metadata (status ${response.statusCode})",
          uri: uri,
        );
      }

      List<int> buffer = <int>[];
      await for (List<int> chunk in response) {
        buffer.addAll(chunk);
      }
      dynamic decoded = jsonDecode(utf8.decode(buffer));
      if (decoded is Map<String, dynamic>) return decoded;
      if (decoded is Map) return decoded.cast<String, dynamic>();
      throw FormatException("Expected JSON object at $uri");
    } finally {
      client.close(force: true);
    }
  }

  Future<Map<String, dynamic>> _readJsonFile(File file) async {
    String content = await file.readAsString();
    dynamic decoded = jsonDecode(content);
    if (decoded is Map<String, dynamic>) return decoded;
    if (decoded is Map) return decoded.cast<String, dynamic>();
    throw FormatException("Expected JSON object in ${file.path}");
  }

  Future<List<dynamic>> _readJsonListFromUri(Uri uri) async {
    HttpClient client = HttpClient();
    client.connectionTimeout = Duration(seconds: 30);
    try {
      HttpClientRequest request = await client.getUrl(uri);
      HttpClientResponse response = await request.close();
      if (response.statusCode != HttpStatus.ok) {
        throw HttpException(
          "Failed to download metadata (status ${response.statusCode})",
          uri: uri,
        );
      }

      List<int> buffer = <int>[];
      await for (List<int> chunk in response) {
        buffer.addAll(chunk);
      }
      dynamic decoded = jsonDecode(utf8.decode(buffer));
      if (decoded is List) return decoded;
      throw FormatException("Expected JSON array at $uri");
    } finally {
      client.close(force: true);
    }
  }

  Future<_PackTagRef> _resolveLatestPackTag() async {
    List<_PackTagRef> tags = await _fetchPackTags();
    if (tags.isEmpty) {
      throw Exception("No tags were returned for the pack repository");
    }
    return tags.first;
  }

  Future<List<_PackTagRef>> _fetchPackTags() async {
    Uri tagsUri = Uri.parse(PackConstants.packTagsApiUrl);
    List<dynamic> values = await _readJsonListFromUri(tagsUri);
    List<_PackTagRef> tags = <_PackTagRef>[];

    for (dynamic value in values) {
      Map<String, dynamic> tagMap = PackJsonUtils.map(value);
      if (tagMap.isEmpty) continue;
      String name = tagMap["name"]?.toString() ?? "";
      if (name.isEmpty) continue;

      Map<String, dynamic> commit = PackJsonUtils.map(tagMap["commit"]);
      String sha = commit["sha"]?.toString() ?? "";
      if (sha.isEmpty) continue;

      tags.add(_PackTagRef(name: name, sha: sha));
    }

    tags.sort(_comparePackTags);
    return tags;
  }

  int _comparePackTags(_PackTagRef a, _PackTagRef b) {
    (int, int, int, bool, String)? aVersion = _parsePackVersion(a.name);
    (int, int, int, bool, String)? bVersion = _parsePackVersion(b.name);

    if (aVersion != null && bVersion != null) {
      if (aVersion.$1 != bVersion.$1) {
        return bVersion.$1.compareTo(aVersion.$1);
      }
      if (aVersion.$2 != bVersion.$2) {
        return bVersion.$2.compareTo(aVersion.$2);
      }
      if (aVersion.$3 != bVersion.$3) {
        return bVersion.$3.compareTo(aVersion.$3);
      }
      if (aVersion.$4 != bVersion.$4) {
        if (aVersion.$4) return 1;
        return -1;
      }
      if (aVersion.$5 != bVersion.$5) {
        return bVersion.$5.compareTo(aVersion.$5);
      }
    } else if (aVersion != null) {
      return -1;
    } else if (bVersion != null) {
      return 1;
    }

    String aName = a.name.toLowerCase();
    String bName = b.name.toLowerCase();
    return bName.compareTo(aName);
  }

  (int, int, int, bool, String)? _parsePackVersion(String rawVersion) {
    String value = rawVersion.trim();
    RegExp regex = RegExp(
      r"^v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$",
    );
    RegExpMatch? match = regex.firstMatch(value);
    if (match == null) return null;

    int? majorCandidate = int.tryParse(match.group(1) ?? "");
    if (majorCandidate == null) return null;
    int major = majorCandidate;

    int minor = 0;
    String minorText = match.group(2) ?? "";
    if (minorText.isNotEmpty) {
      int? minorCandidate = int.tryParse(minorText);
      if (minorCandidate == null) return null;
      minor = minorCandidate;
    }

    int patch = 0;
    String patchText = match.group(3) ?? "";
    if (patchText.isNotEmpty) {
      int? patchCandidate = int.tryParse(patchText);
      if (patchCandidate == null) return null;
      patch = patchCandidate;
    }

    String preRelease = match.group(4) ?? "";
    bool isPreRelease = preRelease.isNotEmpty;
    return (major, minor, patch, isPreRelease, preRelease);
  }

  String _normalizeProtectedPathSpec(String rawSpec) {
    String normalized = rawSpec.trim().replaceAll("\\", "/");
    while (normalized.startsWith("/")) {
      normalized = normalized.substring(1);
    }
    return normalized;
  }

  Future<Directory> _backupProtectedMinecraftFiles() async {
    Directory backupDir = Directory(
      PackPathUtils.joinPath(<String>[tempDir.path, "protected_minecraft"]),
    );
    if (await backupDir.exists()) {
      await backupDir.delete(recursive: true);
    }
    await backupDir.create(recursive: true);

    if (!await minecraftDir.exists()) {
      return backupDir;
    }

    for (String rawSpec in PackConstants.protectedMinecraftPaths) {
      String normalizedSpec = _normalizeProtectedPathSpec(rawSpec);
      if (normalizedSpec.isEmpty) continue;

      bool isDirectorySpec = normalizedSpec.endsWith("/");
      String relativeSpec = normalizedSpec;
      if (isDirectorySpec) {
        relativeSpec = relativeSpec.substring(0, relativeSpec.length - 1);
      }
      if (relativeSpec.isEmpty) continue;

      String localPath = PackPathUtils.toPlatformPath(relativeSpec);
      String sourcePath = PackPathUtils.joinPath(<String>[
        minecraftDir.path,
        localPath,
      ]);
      String backupPath = PackPathUtils.joinPath(<String>[
        backupDir.path,
        localPath,
      ]);

      if (isDirectorySpec) {
        Directory sourceDirectory = Directory(sourcePath);
        if (!await sourceDirectory.exists()) continue;
        await PackFileUtils.copyEntity(
          source: sourceDirectory,
          destination: backupPath,
        );
        continue;
      }

      File sourceFile = File(sourcePath);
      if (await sourceFile.exists()) {
        Directory backupParent = Directory(backupPath).parent;
        if (!await backupParent.exists()) {
          await backupParent.create(recursive: true);
        }
        await PackFileUtils.copyEntity(
          source: sourceFile,
          destination: backupPath,
        );
        continue;
      }

      Directory sourceDirectory = Directory(sourcePath);
      if (!await sourceDirectory.exists()) continue;
      await PackFileUtils.copyEntity(
        source: sourceDirectory,
        destination: backupPath,
      );
    }

    return backupDir;
  }

  Future<void> _restoreProtectedMinecraftFiles(Directory backupDir) async {
    if (!await backupDir.exists()) return;
    if (!await minecraftDir.exists()) {
      await minecraftDir.create(recursive: true);
    }

    await for (FileSystemEntity child in backupDir.list(followLinks: false)) {
      await PackFileUtils.copyEntity(
        source: child,
        destination: PackPathUtils.joinPath(<String>[
          minecraftDir.path,
          PackPathUtils.basename(child.path),
        ]),
      );
    }
  }

  Future<void> _downloadAndInstallPackTag(_PackTagRef tag) async {
    Uri downloadUri = Uri.parse(PackConstants.packTagZipUrl(tag.name));
    File tempZip = File(
      "${tempDir.absolute.path}${Platform.pathSeparator}pack.zip",
    );
    Directory extractDir = Directory(
      "${tempDir.absolute.path}${Platform.pathSeparator}pack_extract",
    );

    if (await tempZip.exists()) {
      await tempZip.delete();
    }
    if (await extractDir.exists()) {
      await extractDir.delete(recursive: true);
    }
    await extractDir.create(recursive: true);

    verbose("Downloading auram ${tag.name} (${tag.sha}) from $downloadUri");
    await _downloadFile(
      uri: downloadUri,
      target: tempZip,
      progressLabel: "Downloading Auram ${tag.name}",
      itemName: "pack archive",
      assumedTotalBytes: PackConstants.assumedPackBytes,
    );

    progressStream.add(("Installing Pack", -1));
    verbose("Extracting pack ${tag.name} into ${gameDir.absolute.path}");
    await PackFileUtils.extractArchive(
      tempZip: tempZip,
      extractDir: extractDir,
    );
    await _installExtractedDirectory(
      extractDir: extractDir,
      installDir: gameDir,
    );

    if (await tempZip.exists()) {
      await tempZip.delete();
    }
    if (await extractDir.exists()) {
      await extractDir.delete(recursive: true);
    }
  }

  String _currentOsName() => switch (currentPlatform) {
    APlatform.macos => "osx",
    APlatform.windows => "windows",
    APlatform.linux => "linux",
  };

  String _ruleArch() => switch (currentArch) {
    AArch.x64 => "x86_64",
    AArch.arm64 => "aarch64",
  };

  String _nativeArchToken() => switch (currentArch) {
    AArch.x64 => "64",
    AArch.arm64 => "arm64",
  };

  bool _matchesRule(Map<String, dynamic> rule) {
    Map<String, dynamic> os = PackJsonUtils.map(rule["os"]);
    if (os.isEmpty) return true;

    String name = os["name"]?.toString() ?? "";
    if (name.isNotEmpty && name != _currentOsName()) return false;

    String archPattern = os["arch"]?.toString() ?? "";
    if (archPattern.isNotEmpty && !RegExp(archPattern).hasMatch(_ruleArch())) {
      return false;
    }

    return true;
  }

  bool _isAllowedByRules(Map<String, dynamic> entry) {
    List<dynamic> rules = PackJsonUtils.list(entry["rules"]);
    if (rules.isEmpty) return true;

    bool allowed = false;
    for (dynamic dynamicRule in rules) {
      Map<String, dynamic> rule = PackJsonUtils.map(dynamicRule);
      if (rule.isEmpty) continue;
      if (!_matchesRule(rule)) continue;
      allowed = rule["action"]?.toString() == "allow";
    }
    return allowed;
  }

  String? _resolveNativeClassifier(Map<String, dynamic> library) {
    Map<String, dynamic> natives = PackJsonUtils.map(library["natives"]);
    if (natives.isEmpty) return null;

    String classifier = natives[_currentOsName()]?.toString() ?? "";
    if (classifier.isEmpty) return null;
    return classifier.replaceAll(r'${arch}', _nativeArchToken());
  }

  String? _mavenPathFromName(String coordinate) {
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

  Uri _libraryBaseUri(Map<String, dynamic> library) {
    String base = library["url"]?.toString() ?? "";
    if (base.isEmpty) base = PackConstants.mojangLibraryBaseUrl;
    if (!base.endsWith("/")) base = "$base/";
    return Uri.parse(base);
  }

  String _forgeInstallerCoordinate(
    String minecraftVersion,
    String forgeVersion,
  ) => "$minecraftVersion-$forgeVersion";

  List<_DownloadTarget> _forgeRuntimeDownloadTargets({
    required String minecraftVersion,
    required String forgeVersion,
    required String mcpVersion,
  }) {
    String mcpCoordinate = "$minecraftVersion-$mcpVersion";
    String forgeCoordinate = "$minecraftVersion-$forgeVersion";
    List<File> files = _forgeRuntimeFiles(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      mcpVersion: mcpVersion,
    );
    List<Uri> uris = <Uri>[
      Uri.parse(
        "${PackConstants.forgeMavenBaseUrl}"
        "net/minecraft/client/$mcpCoordinate/client-$mcpCoordinate-srg.jar",
      ),
      Uri.parse(
        "${PackConstants.forgeMavenBaseUrl}"
        "net/minecraft/client/$mcpCoordinate/client-$mcpCoordinate-extra.jar",
      ),
      Uri.parse(
        "${PackConstants.forgeMavenBaseUrl}"
        "net/minecraftforge/forge/$forgeCoordinate/forge-$forgeCoordinate-client.jar",
      ),
    ];

    List<_DownloadTarget> targets = <_DownloadTarget>[];
    int count = files.length;
    for (int i = 0; i < count; i++) {
      targets.add(_DownloadTarget(uri: uris[i], file: files[i], size: 0));
    }
    return targets;
  }

  Future<File> _downloadForgeInstaller({
    required String minecraftVersion,
    required String forgeVersion,
    required String progressLabel,
  }) async {
    String forgeCoordinate = _forgeInstallerCoordinate(
      minecraftVersion,
      forgeVersion,
    );
    String installerPath =
        "net/minecraftforge/forge/$forgeCoordinate/forge-$forgeCoordinate-installer.jar";
    Uri installerUri = Uri.parse(
      "${PackConstants.forgeMavenBaseUrl}$installerPath",
    );
    File installerFile = File(
      "${tempDir.path}${Platform.pathSeparator}forge-installer-$forgeCoordinate.jar",
    );

    await _downloadFile(
      uri: installerUri,
      target: installerFile,
      progressLabel: progressLabel,
      itemName: "Forge installer",
    );
    return installerFile;
  }

  Future<bool> _downloadForgeRuntimeTarget(_DownloadTarget target) async {
    try {
      await _downloadFile(
        uri: target.uri,
        target: target.file,
        progressLabel: "Downloading Forge Runtime Files",
        itemName: "Forge runtime file",
        emitByteProgress: false,
      );
      return true;
    } catch (_) {
      if (await target.file.exists()) {
        await target.file.delete();
      }
      return false;
    }
  }

  Future<bool> _tryDownloadForgeRuntimeFromMaven({
    required String minecraftVersion,
    required String forgeVersion,
    required String mcpVersion,
  }) async {
    List<_DownloadTarget> allTargets = _forgeRuntimeDownloadTargets(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      mcpVersion: mcpVersion,
    );
    List<_DownloadTarget> missingTargets = <_DownloadTarget>[];
    for (_DownloadTarget target in allTargets) {
      if (await target.file.exists()) continue;
      missingTargets.add(target);
    }
    if (missingTargets.isEmpty) return true;

    int total = missingTargets.length;
    int completed = 0;
    bool allDownloaded = true;
    progressStream.add(("0 forge files downloaded / $total", 0));

    List<Future<void>> futures = <Future<void>>[];
    for (_DownloadTarget target in missingTargets) {
      Future<void> future = _downloadForgeRuntimeTarget(target).then((success) {
        if (!success) {
          allDownloaded = false;
          return;
        }

        completed += 1;
        progressStream.add((
          "$completed forge files downloaded / $total",
          completed / total,
        ));
      });
      futures.add(future);
    }
    await Future.wait(futures);
    if (!allDownloaded) return false;

    for (_DownloadTarget target in allTargets) {
      if (await target.file.exists()) continue;
      return false;
    }
    return true;
  }

  Future<void> _ensureForgeLauncherProfiles({
    required String minecraftVersion,
    required String forgeVersion,
  }) async {
    if (!await minecraftDir.exists()) {
      await minecraftDir.create(recursive: true);
    }

    String versionId = "$minecraftVersion-forge-$forgeVersion";
    String now = DateTime.now().toUtc().toIso8601String();
    Map<String, dynamic> launcherProfile = <String, dynamic>{
      "profiles": <String, dynamic>{
        "Auram": <String, dynamic>{
          "name": "Auram",
          "type": "custom",
          "created": now,
          "lastUsed": now,
          "icon": "Furnace",
          "gameDir": minecraftDir.path,
          "lastVersionId": versionId,
        },
      },
      "selectedProfile": "Auram",
      "clientToken": "",
      "authenticationDatabase": <String, dynamic>{},
      "settings": <String, dynamic>{},
      "version": 3,
    };

    File launcherProfiles = File(
      PackPathUtils.joinPath(<String>[
        minecraftDir.path,
        "launcher_profiles.json",
      ]),
    );
    if (!await launcherProfiles.exists()) {
      await launcherProfiles.writeAsString(jsonEncode(launcherProfile));
    }

    File launcherProfilesStore = File(
      PackPathUtils.joinPath(<String>[
        minecraftDir.path,
        "launcher_profiles_microsoft_store.json",
      ]),
    );
    if (!await launcherProfilesStore.exists()) {
      await launcherProfilesStore.writeAsString(jsonEncode(launcherProfile));
    }
  }

  Future<(String, String)> _readPackVersions() async {
    File packMeta = File(
      "${gameDir.path}${Platform.pathSeparator}mmc-pack.json",
    );
    if (!await packMeta.exists()) {
      throw Exception("Missing mmc-pack.json in ${gameDir.path}");
    }

    Map<String, dynamic> packJson = await _readJsonFile(packMeta);
    List<dynamic> components = PackJsonUtils.list(packJson["components"]);
    String minecraftVersion = "";
    String forgeVersion = "";

    for (dynamic dynamicComponent in components) {
      Map<String, dynamic> component = PackJsonUtils.map(dynamicComponent);
      if (component.isEmpty) continue;
      String uid = component["uid"]?.toString() ?? "";
      String version = component["version"]?.toString() ?? "";
      if (uid == "net.minecraft") {
        minecraftVersion = version;
      } else if (uid == "net.minecraftforge") {
        forgeVersion = version;
      }
    }

    if (minecraftVersion.isEmpty || forgeVersion.isEmpty) {
      throw Exception(
        "Failed to resolve Minecraft/Forge versions from mmc-pack.json",
      );
    }

    return (minecraftVersion, forgeVersion);
  }

  Future<Map<String, dynamic>> _resolveMinecraftVersionJson(
    String minecraftVersion,
  ) async {
    Map<String, dynamic> manifest = await _readJsonFromUri(
      Uri.parse(PackConstants.mojangVersionManifestUrl),
    );
    List<dynamic> versions = PackJsonUtils.list(manifest["versions"]);
    String versionUrl = "";
    for (dynamic dynamicVersion in versions) {
      Map<String, dynamic> version = PackJsonUtils.map(dynamicVersion);
      if (version["id"]?.toString() == minecraftVersion) {
        versionUrl = version["url"]?.toString() ?? "";
        break;
      }
    }
    if (versionUrl.isEmpty) {
      throw Exception("Minecraft version $minecraftVersion was not found");
    }
    return _readJsonFromUri(Uri.parse(versionUrl));
  }

  Future<Map<String, dynamic>> _resolveForgeVersionJson({
    required String minecraftVersion,
    required String forgeVersion,
  }) async {
    File installerFile = await _downloadForgeInstaller(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      progressLabel: "Downloading Forge Installer",
    );

    List<int> installerBytes = await installerFile.readAsBytes();
    Archive installerArchive = ZipDecoder().decodeBytes(installerBytes);
    ArchiveFile? versionFile = installerArchive.findFile("version.json");
    if (versionFile == null) {
      throw Exception("Forge installer did not contain version.json");
    }
    List<int>? raw = versionFile.readBytes();
    if (raw == null) {
      throw Exception("Forge installer version.json could not be read");
    }
    dynamic decoded = jsonDecode(utf8.decode(raw));
    if (decoded is Map<String, dynamic>) return decoded;
    if (decoded is Map) return decoded.cast<String, dynamic>();
    throw Exception("Forge version.json has invalid format");
  }

  File _minecraftVersionJsonFile(String minecraftVersion) => File(
    "${versionsDir.path}${Platform.pathSeparator}$minecraftVersion${Platform.pathSeparator}$minecraftVersion.json",
  );

  Future<Map<String, dynamic>> _loadMinecraftVersionJson(
    String minecraftVersion,
  ) async {
    File installed = _minecraftVersionJsonFile(minecraftVersion);
    if (await installed.exists()) {
      return _readJsonFile(installed);
    }
    return _resolveMinecraftVersionJson(minecraftVersion);
  }

  Future<Map<String, dynamic>> _loadForgeVersionJson({
    required String minecraftVersion,
    required String forgeVersion,
  }) async {
    String expectedVersionId = "$minecraftVersion-forge-$forgeVersion";
    File expected = _versionJsonFile(expectedVersionId);
    if (await expected.exists()) {
      return _readJsonFile(expected);
    }

    try {
      String installedVersionId = await _resolveForgeVersionId(
        minecraftVersion: minecraftVersion,
        forgeVersion: forgeVersion,
      );
      File installed = _versionJsonFile(installedVersionId);
      if (await installed.exists()) {
        return _readJsonFile(installed);
      }
    } catch (_) {}

    return _resolveForgeVersionJson(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
    );
  }

  Future<void> _ensureVersionFiles({
    required String minecraftVersion,
    required Map<String, dynamic> minecraftVersionJson,
    required Map<String, dynamic> forgeVersionJson,
  }) async {
    Directory vanillaVersionDir = Directory(
      "${versionsDir.path}${Platform.pathSeparator}$minecraftVersion",
    );
    if (!await vanillaVersionDir.exists()) {
      await vanillaVersionDir.create(recursive: true);
    }

    File vanillaVersionJsonFile = File(
      "${vanillaVersionDir.path}${Platform.pathSeparator}$minecraftVersion.json",
    );
    if (!await vanillaVersionJsonFile.exists()) {
      await vanillaVersionJsonFile.writeAsString(
        jsonEncode(minecraftVersionJson),
      );
    }

    Map<String, dynamic> downloads = PackJsonUtils.map(
      minecraftVersionJson["downloads"],
    );
    Map<String, dynamic> clientDownload = PackJsonUtils.map(
      downloads["client"],
    );
    String clientUrl = clientDownload["url"]?.toString() ?? "";
    int clientSize = clientDownload["size"] is num
        ? (clientDownload["size"] as num).toInt()
        : 0;
    if (clientUrl.isEmpty) {
      throw Exception("Minecraft client download URL was missing");
    }
    File clientJar = File(
      "${vanillaVersionDir.path}${Platform.pathSeparator}$minecraftVersion.jar",
    );
    await _downloadFile(
      uri: Uri.parse(clientUrl),
      target: clientJar,
      progressLabel: "Downloading Minecraft Client",
      itemName: "Minecraft client jar",
      expectedTotalBytes: clientSize,
    );

    String forgeVersionId = forgeVersionJson["id"]?.toString() ?? "";
    if (forgeVersionId.isEmpty) {
      throw Exception("Forge version id was missing from version.json");
    }
    Directory forgeVersionDir = Directory(
      "${versionsDir.path}${Platform.pathSeparator}$forgeVersionId",
    );
    if (!await forgeVersionDir.exists()) {
      await forgeVersionDir.create(recursive: true);
    }
    File forgeVersionJsonFile = File(
      "${forgeVersionDir.path}${Platform.pathSeparator}$forgeVersionId.json",
    );
    if (!await forgeVersionJsonFile.exists()) {
      await forgeVersionJsonFile.writeAsString(jsonEncode(forgeVersionJson));
    }
  }

  void _addLibraryDownloads({
    required Map<String, dynamic> library,
    required Map<String, _DownloadTarget> outputs,
  }) {
    if (!_isAllowedByRules(library)) return;

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
          outputs[path] = _DownloadTarget(
            uri: Uri.parse(url),
            file: File(
              "${librariesDir.path}${Platform.pathSeparator}${PackPathUtils.toPlatformPath(path)}",
            ),
            size: size,
          );
        }
      } else {
        String name = library["name"]?.toString() ?? "";
        String? path = _mavenPathFromName(name);
        if (path != null) {
          Uri base = _libraryBaseUri(library);
          outputs[path] = _DownloadTarget(
            uri: base.resolve(path),
            file: File(
              "${librariesDir.path}${Platform.pathSeparator}${PackPathUtils.toPlatformPath(path)}",
            ),
            size: 0,
          );
        }
      }

      String? nativeClassifier = _resolveNativeClassifier(library);
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
          outputs[path] = _DownloadTarget(
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
    String? path = _mavenPathFromName(name);
    if (path == null) return;

    Uri base = _libraryBaseUri(library);
    outputs[path] = _DownloadTarget(
      uri: base.resolve(path),
      file: File(
        "${librariesDir.path}${Platform.pathSeparator}${PackPathUtils.toPlatformPath(path)}",
      ),
      size: 0,
    );
  }

  Future<void> _ensureLibraries({
    required List<dynamic> minecraftLibraries,
    required List<dynamic> forgeLibraries,
  }) async {
    if (!await librariesDir.exists()) {
      await librariesDir.create(recursive: true);
    }

    Map<String, _DownloadTarget> outputs = <String, _DownloadTarget>{};
    for (dynamic dynamicLibrary in minecraftLibraries) {
      Map<String, dynamic> library = PackJsonUtils.map(dynamicLibrary);
      if (library.isEmpty) continue;
      _addLibraryDownloads(library: library, outputs: outputs);
    }
    for (dynamic dynamicLibrary in forgeLibraries) {
      Map<String, dynamic> library = PackJsonUtils.map(dynamicLibrary);
      if (library.isEmpty) continue;
      _addLibraryDownloads(library: library, outputs: outputs);
    }

    List<_DownloadTarget> downloads = outputs.values.toList();
    int total = downloads.length;
    if (total <= 0) {
      progressStream.add(("0 libs downloaded / 0 total libs", 1));
      return;
    }

    int completed = 0;
    progressStream.add(("0 libs downloaded / $total total libs", 0));
    List<Future<void>> futures = <Future<void>>[];
    for (_DownloadTarget target in downloads) {
      Future<void> future =
          _downloadFile(
            uri: target.uri,
            target: target.file,
            progressLabel: "Downloading Libraries",
            itemName: "library",
            expectedTotalBytes: target.size,
            emitByteProgress: false,
          ).then((_) {
            completed += 1;
            progressStream.add((
              "$completed libs downloaded / $total total libs",
              completed / total,
            ));
          });
      futures.add(future);
    }

    await Future.wait(futures);
  }

  File _assetCompletionMarker(String indexId) => File(
    "${assetsDir.path}${Platform.pathSeparator}.assets_complete_$indexId",
  );

  Future<void> _clearAssetCompletionMarkers() async {
    if (!await assetsDir.exists()) return;
    await for (FileSystemEntity entity in assetsDir.list(followLinks: false)) {
      if (entity is! File) continue;
      String name = PackPathUtils.basename(entity.path);
      if (!name.startsWith(".assets_complete_")) continue;
      await entity.delete();
    }
  }

  Future<void> _ensureAssets(Map<String, dynamic> minecraftVersionJson) async {
    Map<String, dynamic> assetIndex = PackJsonUtils.map(
      minecraftVersionJson["assetIndex"],
    );
    String indexId = assetIndex["id"]?.toString() ?? "";
    String indexUrl = assetIndex["url"]?.toString() ?? "";
    int indexSize = assetIndex["size"] is num
        ? (assetIndex["size"] as num).toInt()
        : 0;
    if (indexId.isEmpty || indexUrl.isEmpty) {
      throw Exception("Minecraft assets metadata is missing");
    }

    Directory indexDir = Directory(
      "${assetsDir.path}${Platform.pathSeparator}indexes",
    );
    Directory objectsDir = Directory(
      "${assetsDir.path}${Platform.pathSeparator}objects",
    );
    if (!await indexDir.exists()) {
      await indexDir.create(recursive: true);
    }
    if (!await objectsDir.exists()) {
      await objectsDir.create(recursive: true);
    }

    File indexFile = File(
      "${indexDir.path}${Platform.pathSeparator}$indexId.json",
    );
    File completionMarker = _assetCompletionMarker(indexId);
    if (await completionMarker.exists() && await indexFile.exists()) {
      progressStream.add(("Assets Up To Date", 1));
      return;
    }

    if (!await indexFile.exists()) {
      await _downloadFile(
        uri: Uri.parse(indexUrl),
        target: indexFile,
        progressLabel: "Downloading Asset Index",
        itemName: "asset index",
        expectedTotalBytes: indexSize,
      );
    }

    Map<String, dynamic> indexJson = await _readJsonFile(indexFile);
    Map<String, dynamic> objects = PackJsonUtils.map(indexJson["objects"]);
    List<MapEntry<String, dynamic>> entries = objects.entries.toList();
    List<_DownloadTarget> downloads = <_DownloadTarget>[];
    for (MapEntry<String, dynamic> entry in entries) {
      Map<String, dynamic> object = PackJsonUtils.map(entry.value);
      String hash = object["hash"]?.toString() ?? "";
      int size = object["size"] is num ? (object["size"] as num).toInt() : 0;
      if (hash.length < 2) continue;
      String prefix = hash.substring(0, 2);
      Uri objectUri = Uri.parse(
        "${PackConstants.mojangAssetObjectBaseUrl}/$prefix/$hash",
      );
      File objectFile = File(
        "${objectsDir.path}${Platform.pathSeparator}$prefix${Platform.pathSeparator}$hash",
      );
      bool needsDownload = true;
      if (await objectFile.exists()) {
        if (size <= 0) {
          needsDownload = false;
        } else {
          int length = await objectFile.length();
          needsDownload = length != size;
        }
      }
      if (!needsDownload) continue;

      downloads.add(
        _DownloadTarget(uri: objectUri, file: objectFile, size: size),
      );
    }

    int total = downloads.length;
    if (total <= 0) {
      await _clearAssetCompletionMarkers();
      await completionMarker.writeAsString("ok");
      progressStream.add(("Assets Up To Date", 1));
      return;
    }

    int completed = 0;
    int downloading = 0;
    int index = 0;
    List<Future<void>> futures = <Future<void>>[];
    progressStream.add(("0 assets downloaded / $total assets to download", 0));

    while (index < total) {
      if (downloading >= 64) {
        await Future.delayed(Duration(milliseconds: 1));
        continue;
      }

      _DownloadTarget target = downloads[index];
      index += 1;
      downloading += 1;
      Future<void> future =
          _downloadFile(
                uri: target.uri,
                target: target.file,
                progressLabel: "Downloading Assets",
                itemName: "asset object",
                expectedTotalBytes: target.size,
                emitByteProgress: false,
              )
              .then((_) {
                completed += 1;
                progressStream.add((
                  "$completed assets downloaded / $total assets to download",
                  completed / total,
                ));
              })
              .whenComplete(() {
                downloading -= 1;
              });
      futures.add(future);
    }

    await Future.wait(futures);
    await _clearAssetCompletionMarkers();
    await completionMarker.writeAsString("ok");
  }

  Future<void> ensureMinecraftFiles() async {
    progressStream.add(("Resolving Minecraft Runtime", -1));
    (String, String) versions = await _readPackVersions();
    String minecraftVersion = versions.$1;
    String forgeVersion = versions.$2;

    Map<String, dynamic> minecraftVersionJson = await _loadMinecraftVersionJson(
      minecraftVersion,
    );
    Map<String, dynamic> forgeVersionJson = await _loadForgeVersionJson(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
    );

    await _ensureVersionFiles(
      minecraftVersion: minecraftVersion,
      minecraftVersionJson: minecraftVersionJson,
      forgeVersionJson: forgeVersionJson,
    );

    List<dynamic> minecraftLibraries = PackJsonUtils.list(
      minecraftVersionJson["libraries"],
    );
    List<dynamic> forgeLibraries = PackJsonUtils.list(
      forgeVersionJson["libraries"],
    );
    await _ensureLibraries(
      minecraftLibraries: minecraftLibraries,
      forgeLibraries: forgeLibraries,
    );
    await _ensureForgeRuntimeArtifacts(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      forgeVersionJson: forgeVersionJson,
    );
    await _ensureAssets(minecraftVersionJson);
  }

  String _resolveForgeMcpVersion(Map<String, dynamic> forgeVersionJson) {
    Map<String, dynamic> data = PackJsonUtils.map(forgeVersionJson["data"]);
    Map<String, dynamic> mcp = PackJsonUtils.map(data["MCP_VERSION"]);
    String mcpVersion = mcp["client"]?.toString() ?? "";
    if (mcpVersion.isNotEmpty) return mcpVersion;
    mcpVersion = mcp["value"]?.toString() ?? "";
    if (mcpVersion.isNotEmpty) return mcpVersion;

    List<String> gameArgs = _collectVersionArguments(
      versionJson: forgeVersionJson,
      side: "game",
      features: _defaultLaunchFeatures(),
    );
    if (gameArgs.isEmpty) {
      gameArgs = _collectLegacyGameArguments(forgeVersionJson);
    }

    for (int i = 0; i < gameArgs.length; i++) {
      String arg = gameArgs[i];
      if (arg == "--fml.mcpVersion" && i + 1 < gameArgs.length) {
        String value = gameArgs[i + 1];
        if (value.isNotEmpty) return value;
      }

      if (arg.startsWith("--fml.mcpVersion=")) {
        String value = arg.substring("--fml.mcpVersion=".length);
        if (value.isNotEmpty) return value;
      }
    }

    throw Exception("Failed to resolve Forge MCP version");
  }

  List<File> _forgeRuntimeFiles({
    required String minecraftVersion,
    required String forgeVersion,
    required String mcpVersion,
  }) {
    String mcpCoordinate = "$minecraftVersion-$mcpVersion";
    String forgeCoordinate = "$minecraftVersion-$forgeVersion";
    return <File>[
      File(
        PackPathUtils.joinPath(<String>[
          librariesDir.path,
          "net",
          "minecraft",
          "client",
          mcpCoordinate,
          "client-$mcpCoordinate-srg.jar",
        ]),
      ),
      File(
        PackPathUtils.joinPath(<String>[
          librariesDir.path,
          "net",
          "minecraft",
          "client",
          mcpCoordinate,
          "client-$mcpCoordinate-extra.jar",
        ]),
      ),
      File(
        PackPathUtils.joinPath(<String>[
          librariesDir.path,
          "net",
          "minecraftforge",
          "forge",
          forgeCoordinate,
          "forge-$forgeCoordinate-client.jar",
        ]),
      ),
    ];
  }

  Future<void> _ensureForgeRuntimeArtifacts({
    required String minecraftVersion,
    required String forgeVersion,
    required Map<String, dynamic> forgeVersionJson,
  }) async {
    String mcpVersion = _resolveForgeMcpVersion(forgeVersionJson);
    List<File> requiredFiles = _forgeRuntimeFiles(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      mcpVersion: mcpVersion,
    );

    bool allPresent = true;
    for (File file in requiredFiles) {
      if (await file.exists()) continue;
      allPresent = false;
      break;
    }
    if (allPresent) return;

    bool downloadedFromMaven = await _tryDownloadForgeRuntimeFromMaven(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      mcpVersion: mcpVersion,
    );
    if (downloadedFromMaven) return;

    File installerFile = await _downloadForgeInstaller(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      progressLabel: "Downloading Forge Runtime",
    );

    progressStream.add(("Installing Forge Runtime", -1));
    await _ensureForgeLauncherProfiles(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
    );
    String javaExecutable = await _resolveJavaExecutable();
    await _ensureExecutable(javaExecutable);

    ProcessResult result = await Process.run(javaExecutable, <String>[
      "-jar",
      installerFile.path,
      "--installClient",
      minecraftDir.path,
    ], workingDirectory: tempDir.path);
    if (result.exitCode != 0) {
      String stderr = result.stderr.toString().trim();
      String stdout = result.stdout.toString().trim();
      String details = stderr.isNotEmpty ? stderr : stdout;
      if (details.length > 4000) {
        details = details.substring(details.length - 4000);
      }
      throw Exception("Forge installer failed (${result.exitCode}): $details");
    }

    List<String> missing = <String>[];
    for (File file in requiredFiles) {
      if (await file.exists()) continue;
      missing.add(file.path);
    }
    if (missing.isNotEmpty) {
      throw Exception(
        "Forge runtime files are missing after install: ${missing.join(", ")}",
      );
    }
  }

  File _versionJsonFile(String versionId) => File(
    "${versionsDir.path}${Platform.pathSeparator}$versionId${Platform.pathSeparator}$versionId.json",
  );

  File _versionJarFile(String versionId) => File(
    "${versionsDir.path}${Platform.pathSeparator}$versionId${Platform.pathSeparator}$versionId.jar",
  );

  Future<String> _resolveForgeVersionId({
    required String minecraftVersion,
    required String forgeVersion,
  }) async {
    String expected = "$minecraftVersion-forge-$forgeVersion";
    File expectedFile = _versionJsonFile(expected);
    if (await expectedFile.exists()) return expected;

    if (!await versionsDir.exists()) {
      throw Exception("Versions directory is missing: ${versionsDir.path}");
    }

    List<FileSystemEntity> entries = await versionsDir
        .list(followLinks: false)
        .toList();
    for (FileSystemEntity entry in entries) {
      if (entry is! Directory) continue;
      String candidate = PackPathUtils.basename(entry.path);
      if (!candidate.contains("forge")) continue;
      if (!candidate.contains(forgeVersion)) continue;
      File candidateJson = _versionJsonFile(candidate);
      if (await candidateJson.exists()) return candidate;
    }

    throw Exception(
      "Could not find installed Forge version JSON for $forgeVersion",
    );
  }

  Future<Map<String, dynamic>> _readInstalledVersionJson(
    String versionId,
  ) async {
    File jsonFile = _versionJsonFile(versionId);
    if (!await jsonFile.exists()) {
      throw Exception("Missing version JSON: ${jsonFile.path}");
    }
    return _readJsonFile(jsonFile);
  }

  Map<String, bool> _defaultLaunchFeatures() => <String, bool>{
    "is_demo_user": false,
    "has_custom_resolution": false,
    "has_quick_plays_support": false,
    "is_quick_play_singleplayer": false,
    "is_quick_play_multiplayer": false,
    "is_quick_play_realms": false,
  };

  bool _matchesArgumentRule({
    required Map<String, dynamic> rule,
    required Map<String, bool> features,
  }) {
    if (!_matchesRule(rule)) return false;

    Map<String, dynamic> featureRules = PackJsonUtils.map(rule["features"]);
    if (featureRules.isEmpty) return true;

    for (MapEntry<String, dynamic> entry in featureRules.entries) {
      bool expected = entry.value == true;
      bool actual = features[entry.key] ?? false;
      if (actual != expected) return false;
    }

    return true;
  }

  bool _isArgumentEntryAllowed({
    required Map<String, dynamic> entry,
    required Map<String, bool> features,
  }) {
    List<dynamic> rules = PackJsonUtils.list(entry["rules"]);
    if (rules.isEmpty) return true;

    bool allowed = false;
    for (dynamic dynamicRule in rules) {
      Map<String, dynamic> rule = PackJsonUtils.map(dynamicRule);
      if (rule.isEmpty) continue;
      if (!_matchesArgumentRule(rule: rule, features: features)) continue;
      allowed = rule["action"]?.toString() == "allow";
    }

    return allowed;
  }

  void _appendArgumentValue({
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

  void _appendArgumentEntry({
    required List<String> output,
    required dynamic entry,
    required Map<String, bool> features,
  }) {
    if (entry == null) return;
    if (entry is String) {
      output.add(entry);
      return;
    }

    Map<String, dynamic> entryMap = PackJsonUtils.map(entry);
    if (entryMap.isEmpty) return;
    if (!_isArgumentEntryAllowed(entry: entryMap, features: features)) return;
    _appendArgumentValue(output: output, value: entryMap["value"]);
  }

  List<String> _collectVersionArguments({
    required Map<String, dynamic> versionJson,
    required String side,
    required Map<String, bool> features,
  }) {
    Map<String, dynamic> arguments = PackJsonUtils.map(
      versionJson["arguments"],
    );
    dynamic sideArguments = arguments[side];
    if (sideArguments is! List) return <String>[];

    List<String> output = <String>[];
    for (dynamic entry in sideArguments) {
      _appendArgumentEntry(output: output, entry: entry, features: features);
    }

    return output;
  }

  List<String> _splitArguments(String raw) {
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

  List<String> _collectLegacyGameArguments(Map<String, dynamic> versionJson) {
    String raw = versionJson["minecraftArguments"]?.toString() ?? "";
    return _splitArguments(raw);
  }

  List<String> _mergeArguments({
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

  String _applyPlaceholders({
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

  List<String> _applyPlaceholderList({
    required List<String> input,
    required Map<String, String> values,
  }) {
    List<String> output = <String>[];
    for (String item in input) {
      output.add(_applyPlaceholders(input: item, values: values));
    }
    return output;
  }

  void _appendIgnoreListJar({
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

  bool _hasClasspathArgument(List<String> jvmArgs) {
    for (String arg in jvmArgs) {
      if (arg == "-cp" || arg == "-classpath") return true;
    }
    return false;
  }

  void _addUniquePath({
    required List<String> output,
    required Set<String> seen,
    required String path,
  }) {
    if (!seen.contains(path)) {
      seen.add(path);
      output.add(path);
    }
  }

  Future<List<String>> _collectClasspathEntries({
    required String minecraftVersion,
    required String forgeVersionId,
    required Map<String, dynamic> forgeVersionJson,
    required List<dynamic> minecraftLibraries,
    required List<dynamic> forgeLibraries,
  }) async {
    Map<String, _DownloadTarget> downloadMap = <String, _DownloadTarget>{};
    for (dynamic dynamicLibrary in minecraftLibraries) {
      Map<String, dynamic> library = PackJsonUtils.map(dynamicLibrary);
      if (library.isEmpty) continue;
      _addLibraryDownloads(library: library, outputs: downloadMap);
    }
    for (dynamic dynamicLibrary in forgeLibraries) {
      Map<String, dynamic> library = PackJsonUtils.map(dynamicLibrary);
      if (library.isEmpty) continue;
      _addLibraryDownloads(library: library, outputs: downloadMap);
    }

    Set<String> seen = <String>{};
    List<String> classpath = <String>[];
    for (_DownloadTarget download in downloadMap.values) {
      if (!await download.file.exists()) continue;
      _addUniquePath(output: classpath, seen: seen, path: download.file.path);
    }

    String inheritedJarVersion = forgeVersionJson["jar"]?.toString() ?? "";
    if (inheritedJarVersion.isEmpty) inheritedJarVersion = minecraftVersion;
    File inheritedJar = _versionJarFile(inheritedJarVersion);
    if (await inheritedJar.exists()) {
      _addUniquePath(output: classpath, seen: seen, path: inheritedJar.path);
    }

    File minecraftJar = _versionJarFile(minecraftVersion);
    if (await minecraftJar.exists()) {
      _addUniquePath(output: classpath, seen: seen, path: minecraftJar.path);
    }

    File forgeJar = _versionJarFile(forgeVersionId);
    if (await forgeJar.exists()) {
      _addUniquePath(output: classpath, seen: seen, path: forgeJar.path);
    }

    if (classpath.isEmpty) {
      throw Exception("Classpath is empty after resolving libraries and jars");
    }

    return classpath;
  }

  Future<List<File>> _collectNativeJarFiles({
    required List<dynamic> minecraftLibraries,
    required List<dynamic> forgeLibraries,
  }) async {
    List<File> files = <File>[];
    Set<String> seen = <String>{};
    List<dynamic> combined = <dynamic>[];
    combined.addAll(minecraftLibraries);
    combined.addAll(forgeLibraries);

    for (dynamic dynamicLibrary in combined) {
      Map<String, dynamic> library = PackJsonUtils.map(dynamicLibrary);
      if (library.isEmpty) continue;
      if (!_isAllowedByRules(library)) continue;
      String? classifier = _resolveNativeClassifier(library);
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
          path = _mavenPathFromName("$name:$classifier") ?? "";
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

    return files;
  }

  Future<void> _extractNativeJar({
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

  Future<Directory> _prepareNativesDirectory({
    required List<dynamic> minecraftLibraries,
    required List<dynamic> forgeLibraries,
  }) async {
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

    List<File> nativeJars = await _collectNativeJarFiles(
      minecraftLibraries: minecraftLibraries,
      forgeLibraries: forgeLibraries,
    );
    int total = nativeJars.length;
    int index = 0;
    for (File nativeJar in nativeJars) {
      index += 1;
      progressStream.add(("Extracting Natives ($index/$total)", index / total));
      await _extractNativeJar(jarFile: nativeJar, destination: launchNatives);
    }

    return launchNatives;
  }

  Future<String> _resolveJavaExecutable() async {
    List<String> candidates = <String>[
      if (Platform.isWindows)
        "${javaDir.path}${Platform.pathSeparator}bin${Platform.pathSeparator}java.exe",
      if (Platform.isWindows)
        "${javaDir.path}${Platform.pathSeparator}jre${Platform.pathSeparator}bin${Platform.pathSeparator}java.exe",
      if (!Platform.isWindows)
        "${javaDir.path}${Platform.pathSeparator}bin${Platform.pathSeparator}java",
      if (!Platform.isWindows)
        "${javaDir.path}${Platform.pathSeparator}jre${Platform.pathSeparator}bin${Platform.pathSeparator}java",
      if (Platform.isMacOS)
        "${javaDir.path}${Platform.pathSeparator}Contents${Platform.pathSeparator}Home${Platform.pathSeparator}bin${Platform.pathSeparator}java",
    ];

    for (String path in candidates) {
      File file = File(path);
      if (await file.exists()) return file.path;
    }

    await for (FileSystemEntity entity in javaDir.list(
      recursive: true,
      followLinks: false,
    )) {
      if (entity is! File) continue;
      String fileName = PackPathUtils.basename(entity.path).toLowerCase();
      if (fileName != "java" && fileName != "java.exe") continue;
      String parentName = PackPathUtils.basename(
        entity.parent.path,
      ).toLowerCase();
      if (parentName != "bin") continue;
      return entity.path;
    }

    throw Exception("Could not locate a java executable in ${javaDir.path}");
  }

  Future<void> _ensureExecutable(String binaryPath) async {
    if (Platform.isWindows) return;
    ProcessResult result = await Process.run("chmod", ["+x", binaryPath]);
    if (result.exitCode != 0) {
      throw ProcessException(
        "chmod",
        ["+x", binaryPath],
        result.stderr.toString(),
        result.exitCode,
      );
    }
  }

  Future<List<String>> _collectJvmFlagArguments() async =>
      _splitArguments(await knownFlags);

  Future<List<String>> _buildLaunchArguments({
    required Shafted auth,
    required String minecraftVersion,
    required String forgeVersionId,
    required Map<String, dynamic> minecraftVersionJson,
    required Map<String, dynamic> forgeVersionJson,
    required Directory nativesDirectory,
    required List<dynamic> minecraftLibraries,
    required List<dynamic> forgeLibraries,
  }) async {
    Map<String, bool> features = _defaultLaunchFeatures();
    bool inheritsFromBase =
        (forgeVersionJson["inheritsFrom"]?.toString() ?? "").isNotEmpty;

    List<String> minecraftJvmArgs = _collectVersionArguments(
      versionJson: minecraftVersionJson,
      side: "jvm",
      features: features,
    );
    List<String> forgeJvmArgs = _collectVersionArguments(
      versionJson: forgeVersionJson,
      side: "jvm",
      features: features,
    );
    List<String> minecraftGameArgs = _collectVersionArguments(
      versionJson: minecraftVersionJson,
      side: "game",
      features: features,
    );
    List<String> forgeGameArgs = _collectVersionArguments(
      versionJson: forgeVersionJson,
      side: "game",
      features: features,
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
    allJvmArgs.addAll(await _collectJvmFlagArguments());

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
    return launchArguments;
  }

  Future<void> _startDetachedGameProcess({
    required String javaExecutable,
    required List<String> launchArguments,
    required String workingDirectory,
  }) async {
    _resetGameOutputTracking();

    Process process = await Process.start(
      javaExecutable,
      launchArguments,
      workingDirectory: workingDirectory,
      mode: ProcessStartMode.detached,
    );
    gameProcess = process;
  }

  void _resetGameOutputTracking() {
    gameStdoutSubscription?.cancel();
    gameStderrSubscription?.cancel();
    gameLogSink?.close();
    gameStdoutSubscription = null;
    gameStderrSubscription = null;
    gameLogSink = null;
    gameProcess = null;
  }

  Future<void> _installExtractedDirectory({
    required Directory extractDir,
    required Directory installDir,
  }) async {
    List<Directory> topLevelDirs = await extractDir
        .list(followLinks: false)
        .where((entity) => entity is Directory)
        .cast<Directory>()
        .toList();
    Directory sourceRoot = topLevelDirs.length == 1
        ? topLevelDirs.first
        : extractDir;

    if (await installDir.exists()) {
      await installDir.delete(recursive: true);
    }

    if (sourceRoot.path != extractDir.path) {
      try {
        await sourceRoot.rename(installDir.path);
        return;
      } catch (_) {}
    }

    await installDir.create(recursive: true);
    await for (FileSystemEntity child in sourceRoot.list(followLinks: false)) {
      await PackFileUtils.copyEntity(
        source: child,
        destination:
            "${installDir.path}${Platform.pathSeparator}${PackPathUtils.basename(child.path)}",
      );
    }
  }

  Future<void> openDataFolder() async {
    if (!await launcherDir.exists()) {
      await launcherDir.create(recursive: true);
    }
    String folderPath = launcherDir.absolute.path;
    (String, List<String>) command = switch (currentPlatform) {
      APlatform.windows => ("explorer", <String>[folderPath]),
      APlatform.macos => ("open", <String>[folderPath]),
      APlatform.linux => ("xdg-open", <String>[folderPath]),
    };
    await Process.start(
      command.$1,
      command.$2,
      mode: ProcessStartMode.detached,
    );
  }

  Future<void> forceReinstall() async {
    progressStream.add(("Force Reinstall", -1));
    _resetGameOutputTracking();
    if (await launcherDir.exists()) {
      await launcherDir.delete(recursive: true);
    }
    PackDataUtils.setCurrentVersion("");
    await initialize();
  }

  Future<void> ensureInstall() async {
    await ensureJDK();
    await ensurePack();
    await ensureMinecraftFiles();
  }

  Future<void> ensureJDK() async {
    if (await javaDir.exists()) return;
    String downloadUrl = jdkDownload;
    Uri downloadUri = Uri.parse(downloadUrl);
    File tempZip = File(
      "${tempDir.absolute.path}${Platform.pathSeparator}jdk.zip",
    );
    Directory extractDir = Directory(
      "${tempDir.absolute.path}${Platform.pathSeparator}jdk_extract",
    );

    if (await tempZip.exists()) {
      await tempZip.delete();
    }
    if (await extractDir.exists()) {
      await extractDir.delete(recursive: true);
    }
    await extractDir.create(recursive: true);

    verbose("Downloading JDK from $downloadUrl");
    await _downloadFile(
      uri: downloadUri,
      target: tempZip,
      progressLabel: "Downloading JDK",
      itemName: "JDK archive",
    );

    progressStream.add(("Installing JDK", -1));
    verbose("Extracting JDK into ${javaDir.absolute.path}");
    await PackFileUtils.extractArchive(
      tempZip: tempZip,
      extractDir: extractDir,
    );
    await _installExtractedDirectory(
      extractDir: extractDir,
      installDir: javaDir,
    );

    if (await tempZip.exists()) {
      await tempZip.delete();
    }
    if (await extractDir.exists()) {
      await extractDir.delete(recursive: true);
    }

    verbose("Portable JDK ready: ${javaDir.absolute.path}");
  }

  Future<void> ensurePack() async {
    bool hasLocalPack = false;
    if (await gameDir.exists()) {
      hasLocalPack = !(await gameDir.list(followLinks: false).isEmpty);
    }

    progressStream.add(("Checking Pack Updates", -1));
    _PackTagRef latestTag;
    try {
      latestTag = await _resolveLatestPackTag();
    } on Object catch (error) {
      if (hasLocalPack) {
        warn("Failed to fetch pack tags, using local pack: $error");
        return;
      }
      rethrow;
    }
    verbose("Latest pack tag: ${latestTag.name} (${latestTag.sha})");

    String? currentVersion = PackDataUtils.getCurrentVersion();
    if (hasLocalPack && currentVersion == latestTag.name) {
      return;
    }

    Directory backupDir = await _backupProtectedMinecraftFiles();
    try {
      await _downloadAndInstallPackTag(latestTag);
      await _restoreProtectedMinecraftFiles(backupDir);
      PackDataUtils.setCurrentVersion(latestTag.name);
    } finally {
      if (await backupDir.exists()) {
        await backupDir.delete(recursive: true);
      }
    }

    verbose("Pack ready: ${gameDir.absolute.path} (${latestTag.name})");
  }
}

class _DownloadTarget {
  final Uri uri;
  final File file;
  final int size;

  const _DownloadTarget({
    required this.uri,
    required this.file,
    required this.size,
  });
}

class _PackTagRef {
  final String name;
  final String sha;

  const _PackTagRef({required this.name, required this.sha});
}
