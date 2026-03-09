import 'dart:io';

import 'package:arcane/arcane.dart';
import 'package:auram_launcher/pack/pack_constants.dart';
import 'package:auram_launcher/pack/pack_data_utils.dart';
import 'package:auram_launcher/pack/pack_file_utils.dart';
import 'package:auram_launcher/pack/pack_install_utils.dart';
import 'package:auram_launcher/pack/pack_json_utils.dart';
import 'package:auram_launcher/pack/pack_models.dart';
import 'package:auram_launcher/pack/pack_network_io.dart';
import 'package:auram_launcher/pack/pack_path_utils.dart';
import 'package:fast_log/fast_log.dart';

class PackRepositoryService {
  const PackRepositoryService._();

  static Future<void> ensurePack({
    required BehaviorSubject<(String, double)?> progressStream,
    required Directory tempDir,
    required Directory gameDir,
    required Directory minecraftDir,
  }) async {
    info("Ensuring pack install in ${gameDir.path}");
    bool hasLocalPack = false;
    if (await gameDir.exists()) {
      hasLocalPack = !(await gameDir.list(followLinks: false).isEmpty);
    }
    verbose("Local pack present: $hasLocalPack");

    progressStream.add(("Checking Pack Updates", -1));
    PackTagRef latestTag;
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
      success("Pack already up to date at tag ${latestTag.name}");
      return;
    }

    info(
      "Pack update required: current=${currentVersion ?? "none"} latest=${latestTag.name}",
    );
    Directory backupDir = await _backupProtectedMinecraftFiles(
      tempDir: tempDir,
      minecraftDir: minecraftDir,
    );
    try {
      await _downloadAndInstallPackTag(
        progressStream: progressStream,
        tempDir: tempDir,
        gameDir: gameDir,
        tag: latestTag,
      );
      await _restoreProtectedMinecraftFiles(
        backupDir: backupDir,
        minecraftDir: minecraftDir,
      );
      PackDataUtils.setCurrentVersion(latestTag.name);
      success("Pack updated to ${latestTag.name}");
    } finally {
      if (await backupDir.exists()) {
        await backupDir.delete(recursive: true);
      }
    }

    verbose("Pack ready: ${gameDir.absolute.path} (${latestTag.name})");
  }

  static Future<PackTagRef> _resolveLatestPackTag() async {
    verbose("Resolving latest pack tag from GitHub tags API");
    List<PackTagRef> tags = await _fetchPackTags();
    if (tags.isEmpty) {
      throw Exception("No tags were returned for the pack repository");
    }
    success("Resolved latest pack tag: ${tags.first.name}");
    return tags.first;
  }

  static Future<List<PackTagRef>> _fetchPackTags() async {
    Uri tagsUri = Uri.parse(PackConstants.packTagsApiUrl);
    verbose("Fetching pack tags from $tagsUri");
    List<dynamic> values = await PackNetworkIo.readJsonListFromUri(
      uri: tagsUri,
    );
    List<PackTagRef> tags = <PackTagRef>[];

    for (dynamic value in values) {
      Map<String, dynamic> tagMap = PackJsonUtils.map(value);
      if (tagMap.isEmpty) continue;
      String name = tagMap["name"]?.toString() ?? "";
      if (name.isEmpty) continue;

      Map<String, dynamic> commit = PackJsonUtils.map(tagMap["commit"]);
      String sha = commit["sha"]?.toString() ?? "";
      if (sha.isEmpty) continue;

      tags.add(PackTagRef(name: name, sha: sha));
    }

    tags.sort(_comparePackTags);
    info("Fetched ${tags.length} pack tags from repository");
    return tags;
  }

  static int _comparePackTags(PackTagRef a, PackTagRef b) {
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

  static (int, int, int, bool, String)? _parsePackVersion(String rawVersion) {
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

  static String _normalizeProtectedPathSpec(String rawSpec) {
    String normalized = rawSpec.trim().replaceAll("\\", "/");
    while (normalized.startsWith("/")) {
      normalized = normalized.substring(1);
    }
    return normalized;
  }

  static Future<Directory> _backupProtectedMinecraftFiles({
    required Directory tempDir,
    required Directory minecraftDir,
  }) async {
    info("Backing up protected minecraft files from ${minecraftDir.path}");
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

    success("Protected minecraft file backup complete: ${backupDir.path}");
    return backupDir;
  }

  static Future<void> _restoreProtectedMinecraftFiles({
    required Directory backupDir,
    required Directory minecraftDir,
  }) async {
    info("Restoring protected minecraft files from ${backupDir.path}");
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
    success("Protected minecraft files restored to ${minecraftDir.path}");
  }

  static Future<void> _downloadAndInstallPackTag({
    required BehaviorSubject<(String, double)?> progressStream,
    required Directory tempDir,
    required Directory gameDir,
    required PackTagRef tag,
  }) async {
    info("Installing pack tag ${tag.name} (${tag.sha})");
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
    await PackNetworkIo.downloadFile(
      progressStream: progressStream,
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
    await PackInstallUtils.installExtractedDirectory(
      extractDir: extractDir,
      installDir: gameDir,
    );

    if (await tempZip.exists()) {
      await tempZip.delete();
    }
    if (await extractDir.exists()) {
      await extractDir.delete(recursive: true);
    }
    success("Pack tag installed: ${tag.name}");
  }
}
