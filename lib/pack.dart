import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:arcane/arcane.dart';
import 'package:archive/archive.dart';
import 'package:fast_log/fast_log.dart';
import 'package:microshaft/src/model/shafted.dart';
import 'package:path_provider/path_provider.dart';

enum APlatform { macos, windows, linux }

enum AArch { x64, arm64 }

Map<(APlatform, AArch), String> jdkDownloads = {
  (
    APlatform.macos,
    AArch.arm64,
  ): "https://cdn.azul.com/zulu/bin/zulu17.64.17-ca-jdk17.0.18-macosx_aarch64.zip",
  (APlatform.macos, AArch.x64):
      "https://cdn.azul.com/zulu/bin/zulu17.64.17-ca-jdk17.0.18-macosx_x64.zip",
  (APlatform.windows, AArch.x64):
      "https://cdn.azul.com/zulu/bin/zulu17.64.17-ca-jdk17.0.18-win_x64.zip",
  (
    APlatform.windows,
    AArch.arm64,
  ): "https://cdn.azul.com/zulu/bin/zulu17.64.17-ca-jdk17.0.18-win_aarch64.zip",
  (APlatform.linux, AArch.x64):
      "https://cdn.azul.com/zulu/bin/zulu17.64.17-ca-jdk17.0.18-linux_x64.zip",
};

class PackInstance {
  static const int assumedPackBytes = 1024 * 1024 * 1024;
  static const String mojangVersionManifestUrl =
      "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json";
  static const String mojangAssetObjectBaseUrl =
      "https://resources.download.minecraft.net";
  static const String mojangLibraryBaseUrl = "https://libraries.minecraft.net/";
  static const String forgeMavenBaseUrl = "https://maven.minecraftforge.net/";
  static const String jvmFlags =
      "-Xmx16g -Xms8g -XX:+DisableExplicitGC -XX:SoftMaxHeapSize=10g -XX:+UseG1GC -XX:+ParallelRefProcEnabled -XX:MaxGCPauseMillis=16 -XX:+UnlockExperimentalVMOptions -XX:+DisableExplicitGC -XX:+AlwaysPreTouch -XX:G1HeapWastePercent=5 -XX:G1MixedGCCountTarget=4 -XX:InitiatingHeapOccupancyPercent=15 -XX:G1MixedGCLiveThresholdPercent=90 -XX:G1RSetUpdatingPauseTimePercent=5 -XX:SurvivorRatio=32 -XX:MaxTenuringThreshold=1 -XX:G1NewSizePercent=40 -XX:G1MaxNewSizePercent=50 -XX:G1HeapRegionSize=16M -XX:G1ReservePercent=15 -Dfml.readTimeout=120 -Dfml.loginTimeout=120";

  final BehaviorSubject<(String, double)?> progressStream;

  late Directory launcherDir;
  late Directory javaDir;
  late Directory gameDir;
  late Directory tempDir;
  Process? gameProcess;
  StreamSubscription<String>? gameStdoutSubscription;
  StreamSubscription<String>? gameStderrSubscription;
  IOSink? gameLogSink;

  PackInstance() : progressStream = BehaviorSubject.seeded(null);

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
    List<dynamic> minecraftLibraries = _list(minecraftVersionJson["libraries"]);
    List<dynamic> forgeLibraries = _list(forgeVersionJson["libraries"]);
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

  APlatform get currentPlatform {
    if (Platform.isMacOS) return APlatform.macos;
    if (Platform.isWindows) return APlatform.windows;
    if (Platform.isLinux) return APlatform.linux;
    throw UnsupportedError("Unsupported platform");
  }

  AArch get currentArch {
    if (Platform.isMacOS) {
      ProcessResult result = Process.runSync("uname", ["-m"]);
      if (result.exitCode != 0) {
        throw Exception("Failed to determine architecture: ${result.stderr}");
      }
      String arch = result.stdout.toString().trim();
      if (arch == "x86_64") return AArch.x64;
      if (arch == "arm64") return AArch.arm64;
      throw UnsupportedError("Unsupported architecture: $arch");
    } else if (Platform.isWindows || Platform.isLinux) {
      bool isArm64 = Process.runSync("uname", [
        "-m",
      ]).stdout.toString().contains("aarch64");
      return isArm64 ? AArch.arm64 : AArch.x64;
    } else {
      throw UnsupportedError("Unsupported platform");
    }
  }

  String get jdkDownload =>
      jdkDownloads[(currentPlatform, currentArch)] ??
      (throw UnsupportedError(
        "Unsupported platform/architecture combination!",
      ));

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
        (v) => Directory("${v.absolute.path}${Platform.pathSeparator}/Auram"),
      )
      .then((v) async {
        await v.create(recursive: true);
        launcherDir = v;
        tempDir = Directory("${v.absolute.path}${Platform.pathSeparator}temp");
        if (await tempDir.exists()) await tempDir.delete(recursive: true);
        await tempDir.create(recursive: true);
        javaDir = Directory("${v.absolute.path}${Platform.pathSeparator}jvm");
        gameDir = Directory(
          "${v.absolute.path}${Platform.pathSeparator}minecraft",
        );
        verbose("Launcher: ${launcherDir.absolute.path}");
        await ensureInstall();
        progressStream.add(null);
      });

  void dispose() {
    gameStdoutSubscription?.cancel();
    gameStderrSubscription?.cancel();
    gameLogSink?.close();
    gameStdoutSubscription = null;
    gameStderrSubscription = null;
    gameLogSink = null;
    progressStream.close();
  }

  String _basename(String path) {
    List<String> parts = path
        .split(Platform.pathSeparator)
        .where((v) => v.isNotEmpty)
        .toList();
    if (parts.isEmpty) return path;
    return parts.last;
  }

  String _psQuote(String input) => "'${input.replaceAll("'", "''")}'";

  Future<void> _copyEntity({
    required FileSystemEntity source,
    required String destination,
  }) async {
    if (source is File) {
      await source.copy(destination);
      return;
    }

    if (source is Directory) {
      Directory destinationDir = Directory(destination);
      await destinationDir.create(recursive: true);
      await for (FileSystemEntity child in source.list(followLinks: false)) {
        await _copyEntity(
          source: child,
          destination:
              "$destination${Platform.pathSeparator}${_basename(child.path)}",
        );
      }
      return;
    }

    if (source is Link) {
      String target = await source.target();
      await Link(destination).create(target);
    }
  }

  Future<void> _extractArchive({
    required File tempZip,
    required Directory extractDir,
  }) async {
    if (Platform.isWindows) {
      String command =
          "Expand-Archive -LiteralPath ${_psQuote(tempZip.path)} -DestinationPath ${_psQuote(extractDir.path)} -Force";
      ProcessResult result = await Process.run("powershell", [
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        command,
      ]);
      if (result.exitCode != 0) {
        throw ProcessException(
          "powershell",
          ["-Command", command],
          result.stderr.toString(),
          result.exitCode,
        );
      }
      return;
    }

    List<String> unzipArgs = ["-q", "-o", tempZip.path, "-d", extractDir.path];
    ProcessResult unzipResult = await Process.run("unzip", unzipArgs);
    if (unzipResult.exitCode != 0) {
      throw ProcessException(
        "unzip",
        unzipArgs,
        unzipResult.stderr.toString(),
        unzipResult.exitCode,
      );
    }
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

  Map<String, dynamic> _map(dynamic value) {
    if (value is Map<String, dynamic>) return value;
    if (value is Map) return value.cast<String, dynamic>();
    return <String, dynamic>{};
  }

  List<dynamic> _list(dynamic value) {
    if (value is List) return value;
    return <dynamic>[];
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
    Map<String, dynamic> os = _map(rule["os"]);
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
    List<dynamic> rules = _list(entry["rules"]);
    if (rules.isEmpty) return true;

    bool allowed = false;
    for (dynamic dynamicRule in rules) {
      Map<String, dynamic> rule = _map(dynamicRule);
      if (rule.isEmpty) continue;
      if (!_matchesRule(rule)) continue;
      allowed = rule["action"]?.toString() == "allow";
    }
    return allowed;
  }

  String? _resolveNativeClassifier(Map<String, dynamic> library) {
    Map<String, dynamic> natives = _map(library["natives"]);
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
    if (base.isEmpty) base = mojangLibraryBaseUrl;
    if (!base.endsWith("/")) base = "$base/";
    return Uri.parse(base);
  }

  String _forgeInstallerCoordinate(
    String minecraftVersion,
    String forgeVersion,
  ) => "$minecraftVersion-$forgeVersion";

  String _toPlatformPath(String path) =>
      path.replaceAll("/", Platform.pathSeparator);

  Future<(String, String)> _readPackVersions() async {
    File packMeta = File(
      "${gameDir.path}${Platform.pathSeparator}mmc-pack.json",
    );
    if (!await packMeta.exists()) {
      throw Exception("Missing mmc-pack.json in ${gameDir.path}");
    }

    Map<String, dynamic> packJson = await _readJsonFile(packMeta);
    List<dynamic> components = _list(packJson["components"]);
    String minecraftVersion = "";
    String forgeVersion = "";

    for (dynamic dynamicComponent in components) {
      Map<String, dynamic> component = _map(dynamicComponent);
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
      Uri.parse(mojangVersionManifestUrl),
    );
    List<dynamic> versions = _list(manifest["versions"]);
    String versionUrl = "";
    for (dynamic dynamicVersion in versions) {
      Map<String, dynamic> version = _map(dynamicVersion);
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
    String forgeCoordinate = _forgeInstallerCoordinate(
      minecraftVersion,
      forgeVersion,
    );
    String installerPath =
        "net/minecraftforge/forge/$forgeCoordinate/forge-$forgeCoordinate-installer.jar";
    Uri installerUri = Uri.parse("$forgeMavenBaseUrl$installerPath");
    File installerFile = File(
      "${tempDir.path}${Platform.pathSeparator}forge-installer-$forgeCoordinate.jar",
    );

    await _downloadFile(
      uri: installerUri,
      target: installerFile,
      progressLabel: "Downloading Forge Installer",
      itemName: "Forge installer",
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

    Map<String, dynamic> downloads = _map(minecraftVersionJson["downloads"]);
    Map<String, dynamic> clientDownload = _map(downloads["client"]);
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

    Map<String, dynamic> downloads = _map(library["downloads"]);
    if (downloads.isNotEmpty) {
      Map<String, dynamic> artifact = _map(downloads["artifact"]);
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
              "${librariesDir.path}${Platform.pathSeparator}${_toPlatformPath(path)}",
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
              "${librariesDir.path}${Platform.pathSeparator}${_toPlatformPath(path)}",
            ),
            size: 0,
          );
        }
      }

      String? nativeClassifier = _resolveNativeClassifier(library);
      Map<String, dynamic> classifiers = _map(downloads["classifiers"]);
      Map<String, dynamic> nativeArtifact = _map(
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
              "${librariesDir.path}${Platform.pathSeparator}${_toPlatformPath(path)}",
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
        "${librariesDir.path}${Platform.pathSeparator}${_toPlatformPath(path)}",
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
      Map<String, dynamic> library = _map(dynamicLibrary);
      if (library.isEmpty) continue;
      _addLibraryDownloads(library: library, outputs: outputs);
    }
    for (dynamic dynamicLibrary in forgeLibraries) {
      Map<String, dynamic> library = _map(dynamicLibrary);
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

  Future<void> _ensureAssets(Map<String, dynamic> minecraftVersionJson) async {
    Map<String, dynamic> assetIndex = _map(minecraftVersionJson["assetIndex"]);
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
    await _downloadFile(
      uri: Uri.parse(indexUrl),
      target: indexFile,
      progressLabel: "Downloading Asset Index",
      itemName: "asset index",
      expectedTotalBytes: indexSize,
    );

    Map<String, dynamic> indexJson = await _readJsonFile(indexFile);
    Map<String, dynamic> objects = _map(indexJson["objects"]);
    List<MapEntry<String, dynamic>> entries = objects.entries.toList();
    List<_DownloadTarget> downloads = <_DownloadTarget>[];
    for (MapEntry<String, dynamic> entry in entries) {
      Map<String, dynamic> object = _map(entry.value);
      String hash = object["hash"]?.toString() ?? "";
      int size = object["size"] is num ? (object["size"] as num).toInt() : 0;
      if (hash.length < 2) continue;
      String prefix = hash.substring(0, 2);
      Uri objectUri = Uri.parse("$mojangAssetObjectBaseUrl/$prefix/$hash");
      File objectFile = File(
        "${objectsDir.path}${Platform.pathSeparator}$prefix${Platform.pathSeparator}$hash",
      );
      downloads.add(
        _DownloadTarget(uri: objectUri, file: objectFile, size: size),
      );
    }

    int total = downloads.length;
    if (total <= 0) {
      progressStream.add(("0 Assets Installed", 1));
      return;
    }

    int completed = 0;
    int downloading = 0;
    int index = 0;
    List<Future<void>> futures = <Future<void>>[];
    progressStream.add(("Installing $total", 0));

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
                  "$completed assets installed",
                  completed / total,
                ));
              })
              .whenComplete(() {
                downloading -= 1;
              });
      futures.add(future);
    }

    await Future.wait(futures);
  }

  Future<void> ensureMinecraftFiles() async {
    progressStream.add(("Resolving Minecraft Runtime", -1));
    (String, String) versions = await _readPackVersions();
    String minecraftVersion = versions.$1;
    String forgeVersion = versions.$2;

    Map<String, dynamic> minecraftVersionJson =
        await _resolveMinecraftVersionJson(minecraftVersion);
    Map<String, dynamic> forgeVersionJson = await _resolveForgeVersionJson(
      minecraftVersion: minecraftVersion,
      forgeVersion: forgeVersion,
    );

    await _ensureVersionFiles(
      minecraftVersion: minecraftVersion,
      minecraftVersionJson: minecraftVersionJson,
      forgeVersionJson: forgeVersionJson,
    );

    List<dynamic> minecraftLibraries = _list(minecraftVersionJson["libraries"]);
    List<dynamic> forgeLibraries = _list(forgeVersionJson["libraries"]);
    await _ensureLibraries(
      minecraftLibraries: minecraftLibraries,
      forgeLibraries: forgeLibraries,
    );
    await _ensureAssets(minecraftVersionJson);
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
      String candidate = _basename(entry.path);
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

    Map<String, dynamic> featureRules = _map(rule["features"]);
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
    List<dynamic> rules = _list(entry["rules"]);
    if (rules.isEmpty) return true;

    bool allowed = false;
    for (dynamic dynamicRule in rules) {
      Map<String, dynamic> rule = _map(dynamicRule);
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

    Map<String, dynamic> entryMap = _map(entry);
    if (entryMap.isEmpty) return;
    if (!_isArgumentEntryAllowed(entry: entryMap, features: features)) return;
    _appendArgumentValue(output: output, value: entryMap["value"]);
  }

  List<String> _collectVersionArguments({
    required Map<String, dynamic> versionJson,
    required String side,
    required Map<String, bool> features,
  }) {
    Map<String, dynamic> arguments = _map(versionJson["arguments"]);
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
      Map<String, dynamic> library = _map(dynamicLibrary);
      if (library.isEmpty) continue;
      _addLibraryDownloads(library: library, outputs: downloadMap);
    }
    for (dynamic dynamicLibrary in forgeLibraries) {
      Map<String, dynamic> library = _map(dynamicLibrary);
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
      Map<String, dynamic> library = _map(dynamicLibrary);
      if (library.isEmpty) continue;
      if (!_isAllowedByRules(library)) continue;
      String? classifier = _resolveNativeClassifier(library);
      if (classifier == null || classifier.isEmpty) continue;

      Map<String, dynamic> downloads = _map(library["downloads"]);
      Map<String, dynamic> classifiers = _map(downloads["classifiers"]);
      Map<String, dynamic> nativeArtifact = _map(classifiers[classifier]);
      String path = nativeArtifact["path"]?.toString() ?? "";
      if (path.isEmpty) {
        String name = library["name"]?.toString() ?? "";
        if (name.isNotEmpty) {
          path = _mavenPathFromName("$name:$classifier") ?? "";
        }
      }
      if (path.isEmpty) continue;

      String localPath =
          "${librariesDir.path}${Platform.pathSeparator}${_toPlatformPath(path)}";
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
      String localName = _toPlatformPath(entry.name);
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
      String fileName = _basename(entity.path).toLowerCase();
      if (fileName != "java" && fileName != "java.exe") continue;
      String parentName = _basename(entity.parent.path).toLowerCase();
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

  List<String> _collectJvmFlagArguments() => _splitArguments(jvmFlags);

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

    Map<String, dynamic> assetIndex = _map(minecraftVersionJson["assetIndex"]);
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

    if (!_hasClasspathArgument(resolvedJvmArgs)) {
      resolvedJvmArgs.add("-cp");
      resolvedJvmArgs.add(classpath);
    }

    List<String> allJvmArgs = <String>[];
    allJvmArgs.addAll(resolvedJvmArgs);
    allJvmArgs.addAll(_collectJvmFlagArguments());

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
    gameStdoutSubscription?.cancel();
    gameStderrSubscription?.cancel();
    gameLogSink?.close();
    gameStdoutSubscription = null;
    gameStderrSubscription = null;
    gameLogSink = null;

    Directory logDirectory = Directory(
      "${launcherDir.path}${Platform.pathSeparator}logs",
    );
    if (!await logDirectory.exists()) {
      await logDirectory.create(recursive: true);
    }
    String logName = "game-${DateTime.now().millisecondsSinceEpoch}.log";
    File logFile = File(
      "${logDirectory.path}${Platform.pathSeparator}$logName",
    );
    gameLogSink = logFile.openWrite(mode: FileMode.writeOnlyAppend);
    gameLogSink?.writeln("Starting java: $javaExecutable");
    gameLogSink?.writeln("Working directory: $workingDirectory");
    gameLogSink?.writeln("Argument count: ${launchArguments.length}");

    Process process = await Process.start(
      javaExecutable,
      launchArguments,
      workingDirectory: workingDirectory,
      mode: ProcessStartMode.detachedWithStdio,
    );
    gameProcess = process;

    Stream<String> outLines = process.stdout
        .transform(utf8.decoder)
        .transform(LineSplitter());
    Stream<String> errLines = process.stderr
        .transform(utf8.decoder)
        .transform(LineSplitter());

    gameStdoutSubscription = outLines.listen(_onGameStdout);
    gameStderrSubscription = errLines.listen(_onGameStderr);
    process.exitCode.then(_onGameExit);
  }

  void _onGameStdout(String line) {
    verbose("[GAME] $line");
    gameLogSink?.writeln("[OUT] $line");
  }

  void _onGameStderr(String line) {
    verbose("[GAME-ERR] $line");
    gameLogSink?.writeln("[ERR] $line");
  }

  void _onGameExit(int code) {
    verbose("Game exited with code $code");
    gameLogSink?.writeln("Game exited with code $code");
    gameLogSink?.close();
    gameLogSink = null;
    gameStdoutSubscription?.cancel();
    gameStderrSubscription?.cancel();
    gameStdoutSubscription = null;
    gameStderrSubscription = null;
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
      await _copyEntity(
        source: child,
        destination:
            "${installDir.path}${Platform.pathSeparator}${_basename(child.path)}",
      );
    }
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
    await _extractArchive(tempZip: tempZip, extractDir: extractDir);
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
    if (await gameDir.exists()) {
      bool hasContents = !(await gameDir.list(followLinks: false).isEmpty);
      if (hasContents) return;
      await gameDir.delete(recursive: true);
    }

    List<Uri> downloadUris = [
      Uri.parse(
        "https://codeload.github.com/AuramMods/Auram/zip/refs/heads/main",
      ),
      Uri.parse(
        "https://codeload.github.com/AuramMods/Auram/zip/refs/heads/master",
      ),
    ];

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

    bool downloaded = false;
    for (Uri uri in downloadUris) {
      try {
        verbose("Downloading auram from $uri");
        await _downloadFile(
          uri: uri,
          target: tempZip,
          progressLabel: "Downloading Auram",
          itemName: "pack archive",
          assumedTotalBytes: assumedPackBytes,
        );
        downloaded = true;
        break;
      } catch (_) {
        if (await tempZip.exists()) {
          await tempZip.delete();
        }
      }
    }

    if (!downloaded) {
      throw Exception("Failed to download modpack archive from GitHub");
    }

    progressStream.add(("Installing Pack", -1));
    verbose("Extracting pack into ${gameDir.absolute.path}");
    await _extractArchive(tempZip: tempZip, extractDir: extractDir);
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

    verbose("Pack ready: ${gameDir.absolute.path}");
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
