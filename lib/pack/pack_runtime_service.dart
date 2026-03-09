import 'dart:convert';
import 'dart:io';

import 'package:arcane/arcane.dart';
import 'package:archive/archive.dart';
import 'package:auram_launcher/pack/pack_constants.dart';
import 'package:auram_launcher/pack/pack_java_utils.dart';
import 'package:auram_launcher/pack/pack_json_utils.dart';
import 'package:auram_launcher/pack/pack_library_utils.dart';
import 'package:auram_launcher/pack/pack_models.dart';
import 'package:auram_launcher/pack/pack_network_io.dart';
import 'package:auram_launcher/pack/pack_path_utils.dart';
import 'package:auram_launcher/pack/pack_types.dart';
import 'package:fast_log/fast_log.dart';

class PackRuntimeService {
  const PackRuntimeService._();

  static Future<void> ensureMinecraftFiles({
    required BehaviorSubject<(String, double)?> progressStream,
    required Directory tempDir,
    required Directory javaDir,
    required Directory gameDir,
    required Directory minecraftDir,
    required Directory librariesDir,
    required Directory versionsDir,
    required Directory assetsDir,
    required APlatform platform,
    required AArch arch,
  }) async {
    info("Ensuring Minecraft runtime files in ${minecraftDir.path}");
    progressStream.add(("Resolving Minecraft Runtime", -1));
    (String, String) versions = await readPackVersions(gameDir: gameDir);
    String minecraftVersion = versions.$1;
    String forgeVersion = versions.$2;
    verbose(
      "Resolved pack versions: minecraft=$minecraftVersion forge=$forgeVersion",
    );

    Map<String, dynamic> minecraftVersionJson = await _loadMinecraftVersionJson(
      versionsDir: versionsDir,
      minecraftVersion: minecraftVersion,
    );
    Map<String, dynamic> forgeVersionJson = await _loadForgeVersionJson(
      tempDir: tempDir,
      versionsDir: versionsDir,
      progressStream: progressStream,
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
    );

    await _ensureVersionFiles(
      versionsDir: versionsDir,
      progressStream: progressStream,
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
      librariesDir: librariesDir,
      progressStream: progressStream,
      platform: platform,
      arch: arch,
      minecraftLibraries: minecraftLibraries,
      forgeLibraries: forgeLibraries,
    );
    await _ensureForgeRuntimeArtifacts(
      tempDir: tempDir,
      javaDir: javaDir,
      minecraftDir: minecraftDir,
      librariesDir: librariesDir,
      progressStream: progressStream,
      platform: platform,
      arch: arch,
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      forgeVersionJson: forgeVersionJson,
    );
    await _ensureAssets(
      assetsDir: assetsDir,
      progressStream: progressStream,
      minecraftVersionJson: minecraftVersionJson,
    );
    success(
      "Minecraft runtime files ensured for $minecraftVersion / $forgeVersion",
    );
  }

  static Future<(String, String)> readPackVersions({
    required Directory gameDir,
  }) async {
    verbose("Reading pack versions from ${gameDir.path}");
    File packMeta = File(
      "${gameDir.path}${Platform.pathSeparator}mmc-pack.json",
    );
    if (!await packMeta.exists()) {
      throw Exception("Missing mmc-pack.json in ${gameDir.path}");
    }

    Map<String, dynamic> packJson = await PackNetworkIo.readJsonFile(
      file: packMeta,
    );
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

    info(
      "Pack metadata versions: minecraft=$minecraftVersion forge=$forgeVersion",
    );
    return (minecraftVersion, forgeVersion);
  }

  static Future<String> resolveForgeVersionId({
    required Directory versionsDir,
    required String minecraftVersion,
    required String forgeVersion,
  }) async {
    verbose(
      "Resolving Forge version id for minecraft=$minecraftVersion forge=$forgeVersion",
    );
    String expected = "$minecraftVersion-forge-$forgeVersion";
    File expectedFile = _versionJsonFile(
      versionsDir: versionsDir,
      versionId: expected,
    );
    if (await expectedFile.exists()) {
      success("Resolved Forge version id: $expected");
      return expected;
    }

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
      File candidateJson = _versionJsonFile(
        versionsDir: versionsDir,
        versionId: candidate,
      );
      if (await candidateJson.exists()) {
        success("Resolved Forge version id fallback: $candidate");
        return candidate;
      }
    }

    error("Failed to resolve Forge version id for forge=$forgeVersion");
    throw Exception(
      "Could not find installed Forge version JSON for $forgeVersion",
    );
  }

  static Future<Map<String, dynamic>> readInstalledVersionJson({
    required Directory versionsDir,
    required String versionId,
  }) async {
    verbose("Reading installed version JSON for $versionId");
    File jsonFile = _versionJsonFile(
      versionsDir: versionsDir,
      versionId: versionId,
    );
    if (!await jsonFile.exists()) {
      throw Exception("Missing version JSON: ${jsonFile.path}");
    }
    Map<String, dynamic> result = await PackNetworkIo.readJsonFile(
      file: jsonFile,
    );
    success("Loaded installed version JSON: $versionId");
    return result;
  }

  static String _forgeInstallerCoordinate(
    String minecraftVersion,
    String forgeVersion,
  ) => "$minecraftVersion-$forgeVersion";

  static List<DownloadTarget> _forgeRuntimeDownloadTargets({
    required Directory librariesDir,
    required String minecraftVersion,
    required String forgeVersion,
    required String mcpVersion,
  }) {
    String mcpCoordinate = "$minecraftVersion-$mcpVersion";
    String forgeCoordinate = "$minecraftVersion-$forgeVersion";
    List<File> files = _forgeRuntimeFiles(
      librariesDir: librariesDir,
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

    List<DownloadTarget> targets = <DownloadTarget>[];
    int count = files.length;
    for (int i = 0; i < count; i++) {
      targets.add(DownloadTarget(uri: uris[i], file: files[i], size: 0));
    }
    return targets;
  }

  static Future<File> _downloadForgeInstaller({
    required BehaviorSubject<(String, double)?> progressStream,
    required Directory tempDir,
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

    info("Downloading Forge installer $forgeCoordinate");
    await PackNetworkIo.downloadFile(
      progressStream: progressStream,
      uri: installerUri,
      target: installerFile,
      progressLabel: progressLabel,
      itemName: "Forge installer",
    );
    success("Forge installer downloaded: ${installerFile.path}");
    return installerFile;
  }

  static Future<bool> _downloadForgeRuntimeTarget({
    required BehaviorSubject<(String, double)?> progressStream,
    required DownloadTarget target,
  }) async {
    verbose("Downloading Forge runtime target: ${target.uri}");
    try {
      await PackNetworkIo.downloadFile(
        progressStream: progressStream,
        uri: target.uri,
        target: target.file,
        progressLabel: "Downloading Forge Runtime Files",
        itemName: "Forge runtime file",
        emitByteProgress: false,
      );
      success("Forge runtime target downloaded: ${target.file.path}");
      return true;
    } catch (_) {
      warn("Forge runtime target failed: ${target.uri}");
      if (await target.file.exists()) {
        await target.file.delete();
      }
      return false;
    }
  }

  static Future<bool> _tryDownloadForgeRuntimeFromMaven({
    required BehaviorSubject<(String, double)?> progressStream,
    required Directory librariesDir,
    required String minecraftVersion,
    required String forgeVersion,
    required String mcpVersion,
  }) async {
    info("Attempting Forge runtime download from Maven");
    List<DownloadTarget> allTargets = _forgeRuntimeDownloadTargets(
      librariesDir: librariesDir,
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      mcpVersion: mcpVersion,
    );
    List<DownloadTarget> missingTargets = <DownloadTarget>[];
    for (DownloadTarget target in allTargets) {
      if (await target.file.exists()) continue;
      missingTargets.add(target);
    }
    if (missingTargets.isEmpty) return true;
    verbose("Missing Forge runtime targets: ${missingTargets.length}");

    int total = missingTargets.length;
    int completed = 0;
    bool allDownloaded = true;
    progressStream.add(("0 forge files downloaded / $total", 0));

    List<Future<void>> futures = <Future<void>>[];
    for (DownloadTarget target in missingTargets) {
      Future<void> future =
          _downloadForgeRuntimeTarget(
            progressStream: progressStream,
            target: target,
          ).then((success) {
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
    if (!allDownloaded) {
      warn("Forge runtime Maven download was incomplete");
      return false;
    }

    for (DownloadTarget target in allTargets) {
      if (await target.file.exists()) continue;
      warn(
        "Forge runtime target missing after Maven attempt: ${target.file.path}",
      );
      return false;
    }
    success("Forge runtime download from Maven completed");
    return true;
  }

  static Future<void> _ensureForgeLauncherProfiles({
    required Directory minecraftDir,
    required String minecraftVersion,
    required String forgeVersion,
  }) async {
    verbose("Ensuring launcher profile files in ${minecraftDir.path}");
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
      info("Created launcher_profiles.json");
    }

    File launcherProfilesStore = File(
      PackPathUtils.joinPath(<String>[
        minecraftDir.path,
        "launcher_profiles_microsoft_store.json",
      ]),
    );
    if (!await launcherProfilesStore.exists()) {
      await launcherProfilesStore.writeAsString(jsonEncode(launcherProfile));
      info("Created launcher_profiles_microsoft_store.json");
    }
  }

  static Future<Map<String, dynamic>> _resolveMinecraftVersionJson(
    String minecraftVersion,
  ) async {
    info("Resolving Minecraft version metadata for $minecraftVersion");
    Map<String, dynamic> manifest = await PackNetworkIo.readJsonFromUri(
      uri: Uri.parse(PackConstants.mojangVersionManifestUrl),
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
    verbose("Minecraft version metadata URL: $versionUrl");
    return PackNetworkIo.readJsonFromUri(uri: Uri.parse(versionUrl));
  }

  static Future<Map<String, dynamic>> _resolveForgeVersionJson({
    required BehaviorSubject<(String, double)?> progressStream,
    required Directory tempDir,
    required String minecraftVersion,
    required String forgeVersion,
  }) async {
    info(
      "Resolving Forge version metadata for $minecraftVersion-$forgeVersion",
    );
    File installerFile = await _downloadForgeInstaller(
      progressStream: progressStream,
      tempDir: tempDir,
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
    if (decoded is Map<String, dynamic>) {
      success("Resolved Forge version metadata from installer archive");
      return decoded;
    }
    if (decoded is Map) {
      success("Resolved Forge version metadata from installer archive");
      return decoded.cast<String, dynamic>();
    }
    throw Exception("Forge version.json has invalid format");
  }

  static File _minecraftVersionJsonFile({
    required Directory versionsDir,
    required String minecraftVersion,
  }) => File(
    "${versionsDir.path}${Platform.pathSeparator}$minecraftVersion${Platform.pathSeparator}$minecraftVersion.json",
  );

  static Future<Map<String, dynamic>> _loadMinecraftVersionJson({
    required Directory versionsDir,
    required String minecraftVersion,
  }) async {
    verbose("Loading Minecraft version JSON for $minecraftVersion");
    File installed = _minecraftVersionJsonFile(
      versionsDir: versionsDir,
      minecraftVersion: minecraftVersion,
    );
    if (await installed.exists()) {
      success("Using installed Minecraft version JSON: ${installed.path}");
      return PackNetworkIo.readJsonFile(file: installed);
    }
    info("Installed Minecraft version JSON missing, downloading metadata");
    return _resolveMinecraftVersionJson(minecraftVersion);
  }

  static Future<Map<String, dynamic>> _loadForgeVersionJson({
    required BehaviorSubject<(String, double)?> progressStream,
    required Directory tempDir,
    required Directory versionsDir,
    required String minecraftVersion,
    required String forgeVersion,
  }) async {
    verbose(
      "Loading Forge version JSON for $minecraftVersion-forge-$forgeVersion",
    );
    String expectedVersionId = "$minecraftVersion-forge-$forgeVersion";
    File expected = _versionJsonFile(
      versionsDir: versionsDir,
      versionId: expectedVersionId,
    );
    if (await expected.exists()) {
      success("Using installed Forge version JSON: ${expected.path}");
      return PackNetworkIo.readJsonFile(file: expected);
    }

    try {
      String installedVersionId = await resolveForgeVersionId(
        versionsDir: versionsDir,
        minecraftVersion: minecraftVersion,
        forgeVersion: forgeVersion,
      );
      File installed = _versionJsonFile(
        versionsDir: versionsDir,
        versionId: installedVersionId,
      );
      if (await installed.exists()) {
        success(
          "Using installed Forge version JSON fallback: ${installed.path}",
        );
        return PackNetworkIo.readJsonFile(file: installed);
      }
    } catch (_) {}

    info("Installed Forge version JSON missing, resolving from installer");
    return _resolveForgeVersionJson(
      progressStream: progressStream,
      tempDir: tempDir,
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
    );
  }

  static Future<void> _ensureVersionFiles({
    required Directory versionsDir,
    required BehaviorSubject<(String, double)?> progressStream,
    required String minecraftVersion,
    required Map<String, dynamic> minecraftVersionJson,
    required Map<String, dynamic> forgeVersionJson,
  }) async {
    info("Ensuring version files for Minecraft $minecraftVersion");
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
      verbose("Wrote Minecraft version JSON: ${vanillaVersionJsonFile.path}");
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
    await PackNetworkIo.downloadFile(
      progressStream: progressStream,
      uri: Uri.parse(clientUrl),
      target: clientJar,
      progressLabel: "Downloading Minecraft Client",
      itemName: "Minecraft client jar",
      expectedTotalBytes: clientSize,
    );
    success("Minecraft client jar available: ${clientJar.path}");

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
      verbose("Wrote Forge version JSON: ${forgeVersionJsonFile.path}");
    }
    success("Version file ensure completed for Minecraft $minecraftVersion");
  }

  static Future<void> _ensureLibraries({
    required Directory librariesDir,
    required BehaviorSubject<(String, double)?> progressStream,
    required APlatform platform,
    required AArch arch,
    required List<dynamic> minecraftLibraries,
    required List<dynamic> forgeLibraries,
  }) async {
    info("Ensuring Minecraft/Forge library files");
    if (!await librariesDir.exists()) {
      await librariesDir.create(recursive: true);
    }

    Map<String, DownloadTarget> outputs = <String, DownloadTarget>{};
    for (dynamic dynamicLibrary in minecraftLibraries) {
      Map<String, dynamic> library = PackJsonUtils.map(dynamicLibrary);
      if (library.isEmpty) continue;
      PackLibraryUtils.addLibraryDownloads(
        library: library,
        outputs: outputs,
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
        outputs: outputs,
        librariesDir: librariesDir,
        platform: platform,
        arch: arch,
      );
    }

    List<DownloadTarget> downloads = outputs.values.toList();
    int total = downloads.length;
    if (total <= 0) {
      progressStream.add(("0 libs downloaded / 0 total libs", 1));
      success("No libraries required for download");
      return;
    }

    verbose("Library download targets: $total");
    int completed = 0;
    progressStream.add(("0 libs downloaded / $total total libs", 0));
    List<Future<void>> futures = <Future<void>>[];
    for (DownloadTarget target in downloads) {
      Future<void> future =
          PackNetworkIo.downloadFile(
            progressStream: progressStream,
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
    success("Library download completed ($completed/$total)");
  }

  static File _assetCompletionMarker({
    required Directory assetsDir,
    required String indexId,
  }) => File(
    "${assetsDir.path}${Platform.pathSeparator}.assets_complete_$indexId",
  );

  static Future<void> _clearAssetCompletionMarkers({
    required Directory assetsDir,
  }) async {
    verbose("Clearing asset completion markers in ${assetsDir.path}");
    if (!await assetsDir.exists()) return;
    await for (FileSystemEntity entity in assetsDir.list(followLinks: false)) {
      if (entity is! File) continue;
      String name = PackPathUtils.basename(entity.path);
      if (!name.startsWith(".assets_complete_")) continue;
      await entity.delete();
    }
  }

  static Future<void> _ensureAssets({
    required Directory assetsDir,
    required BehaviorSubject<(String, double)?> progressStream,
    required Map<String, dynamic> minecraftVersionJson,
  }) async {
    info("Ensuring Minecraft assets");
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
    verbose("Asset index id=$indexId url=$indexUrl");

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
    File completionMarker = _assetCompletionMarker(
      assetsDir: assetsDir,
      indexId: indexId,
    );
    if (await completionMarker.exists() && await indexFile.exists()) {
      progressStream.add(("Assets Up To Date", 1));
      success("Assets already up to date ($indexId)");
      return;
    }

    if (!await indexFile.exists()) {
      await PackNetworkIo.downloadFile(
        progressStream: progressStream,
        uri: Uri.parse(indexUrl),
        target: indexFile,
        progressLabel: "Downloading Asset Index",
        itemName: "asset index",
        expectedTotalBytes: indexSize,
        maxAttempts: 6,
      );
    }

    Map<String, dynamic> indexJson = await PackNetworkIo.readJsonFile(
      file: indexFile,
    );
    Map<String, dynamic> objects = PackJsonUtils.map(indexJson["objects"]);
    List<MapEntry<String, dynamic>> entries = objects.entries.toList();
    List<DownloadTarget> downloads = <DownloadTarget>[];
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
        DownloadTarget(uri: objectUri, file: objectFile, size: size),
      );
    }

    int total = downloads.length;
    if (total <= 0) {
      await _clearAssetCompletionMarkers(assetsDir: assetsDir);
      await completionMarker.writeAsString("ok");
      progressStream.add(("Assets Up To Date", 1));
      success("Asset index verified with no pending object downloads");
      return;
    }

    verbose("Asset download targets: $total");
    int completed = 0;
    int downloading = 0;
    int index = 0;
    List<Future<void>> futures = <Future<void>>[];
    progressStream.add(("0 assets downloaded / $total assets to download", 0));

    while (index < total) {
      if (downloading >= 32) {
        await Future.delayed(Duration(milliseconds: 1));
        continue;
      }

      DownloadTarget target = downloads[index];
      index += 1;
      downloading += 1;
      Future<void> future =
          PackNetworkIo.downloadFile(
                progressStream: progressStream,
                uri: target.uri,
                target: target.file,
                progressLabel: "Downloading Assets",
                itemName: "asset object",
                expectedTotalBytes: target.size,
                emitByteProgress: false,
                maxAttempts: 8,
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
    await _clearAssetCompletionMarkers(assetsDir: assetsDir);
    await completionMarker.writeAsString("ok");
    success("Asset download completed ($completed/$total)");
  }

  static String _resolveForgeMcpVersion({
    required Map<String, dynamic> forgeVersionJson,
    required APlatform platform,
    required AArch arch,
  }) {
    verbose("Resolving Forge MCP runtime version");
    Map<String, dynamic> data = PackJsonUtils.map(forgeVersionJson["data"]);
    Map<String, dynamic> mcp = PackJsonUtils.map(data["MCP_VERSION"]);
    String mcpVersion = mcp["client"]?.toString() ?? "";
    if (mcpVersion.isNotEmpty) {
      success("Resolved Forge MCP version from data.client: $mcpVersion");
      return mcpVersion;
    }
    mcpVersion = mcp["value"]?.toString() ?? "";
    if (mcpVersion.isNotEmpty) {
      success("Resolved Forge MCP version from data.value: $mcpVersion");
      return mcpVersion;
    }

    List<String> gameArgs = _collectVersionArguments(
      versionJson: forgeVersionJson,
      side: "game",
      features: _defaultLaunchFeatures(),
      platform: platform,
      arch: arch,
    );
    if (gameArgs.isEmpty) {
      gameArgs = _collectLegacyGameArguments(forgeVersionJson);
    }

    for (int i = 0; i < gameArgs.length; i++) {
      String arg = gameArgs[i];
      if (arg == "--fml.mcpVersion" && i + 1 < gameArgs.length) {
        String value = gameArgs[i + 1];
        if (value.isNotEmpty) {
          success("Resolved Forge MCP version from game args: $value");
          return value;
        }
      }

      if (arg.startsWith("--fml.mcpVersion=")) {
        String value = arg.substring("--fml.mcpVersion=".length);
        if (value.isNotEmpty) {
          success("Resolved Forge MCP version from inline arg: $value");
          return value;
        }
      }
    }

    error("Failed to resolve Forge MCP runtime version");
    throw Exception("Failed to resolve Forge MCP version");
  }

  static List<File> _forgeRuntimeFiles({
    required Directory librariesDir,
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

  static Future<void> _ensureForgeRuntimeArtifacts({
    required Directory tempDir,
    required Directory javaDir,
    required Directory minecraftDir,
    required Directory librariesDir,
    required BehaviorSubject<(String, double)?> progressStream,
    required APlatform platform,
    required AArch arch,
    required String minecraftVersion,
    required String forgeVersion,
    required Map<String, dynamic> forgeVersionJson,
  }) async {
    info("Ensuring Forge runtime artifacts");
    String mcpVersion = _resolveForgeMcpVersion(
      forgeVersionJson: forgeVersionJson,
      platform: platform,
      arch: arch,
    );
    List<File> requiredFiles = _forgeRuntimeFiles(
      librariesDir: librariesDir,
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
    if (allPresent) {
      success("Forge runtime artifacts already present");
      return;
    }

    bool downloadedFromMaven = await _tryDownloadForgeRuntimeFromMaven(
      progressStream: progressStream,
      librariesDir: librariesDir,
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      mcpVersion: mcpVersion,
    );
    if (downloadedFromMaven) {
      success("Forge runtime artifacts restored from Maven");
      return;
    }

    warn("Falling back to Forge installer for runtime artifacts");
    File installerFile = await _downloadForgeInstaller(
      progressStream: progressStream,
      tempDir: tempDir,
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
      progressLabel: "Downloading Forge Runtime",
    );

    progressStream.add(("Installing Forge Runtime", -1));
    await _ensureForgeLauncherProfiles(
      minecraftDir: minecraftDir,
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
    );
    String javaExecutable = await PackJavaUtils.resolveJavaExecutable(
      javaDir: javaDir,
    );
    await PackJavaUtils.ensureExecutable(binaryPath: javaExecutable);

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
      error("Forge installer failed with code ${result.exitCode}");
      throw Exception("Forge installer failed (${result.exitCode}): $details");
    }

    List<String> missing = <String>[];
    for (File file in requiredFiles) {
      if (await file.exists()) continue;
      missing.add(file.path);
    }
    if (missing.isNotEmpty) {
      error("Forge runtime artifacts still missing: ${missing.join(", ")}");
      throw Exception(
        "Forge runtime files are missing after install: ${missing.join(", ")}",
      );
    }
    success("Forge runtime artifacts are ready");
  }

  static File _versionJsonFile({
    required Directory versionsDir,
    required String versionId,
  }) => File(
    "${versionsDir.path}${Platform.pathSeparator}$versionId${Platform.pathSeparator}$versionId.json",
  );

  static File versionJarFile({
    required Directory versionsDir,
    required String versionId,
  }) => File(
    "${versionsDir.path}${Platform.pathSeparator}$versionId${Platform.pathSeparator}$versionId.jar",
  );

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
}
