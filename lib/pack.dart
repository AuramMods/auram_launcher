import 'dart:io';

import 'package:arcane/arcane.dart';
import 'package:auram_launcher/pack/pack_constants.dart';
import 'package:auram_launcher/pack/pack_data_utils.dart';
import 'package:auram_launcher/pack/pack_jdk_service.dart';
import 'package:auram_launcher/pack/pack_launch_service.dart';
import 'package:auram_launcher/pack/pack_path_utils.dart';
import 'package:auram_launcher/pack/pack_platform_utils.dart';
import 'package:auram_launcher/pack/pack_repository_service.dart';
import 'package:auram_launcher/pack/pack_runtime_service.dart';
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

  PackInstance()
    : progressStream = BehaviorSubject.seeded(null),
      knownFlags = PackConstants.jvmFlags;

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

  Future<void> launch(Shafted auth) async {
    info("Launch requested");
    await PackLaunchService.launch(
      auth: auth,
      progressStream: progressStream,
      knownFlags: knownFlags,
      javaDir: javaDir,
      gameDir: gameDir,
      minecraftDir: minecraftDir,
      librariesDir: librariesDir,
      versionsDir: versionsDir,
      assetsDir: assetsDir,
      nativesRootDir: nativesRootDir,
      platform: currentPlatform,
      arch: currentArch,
    );
    success("Launch flow finished");
  }

  Future<void> initialize() => getApplicationSupportDirectory()
      .then(
        (v) => Directory(
          PackPathUtils.joinPath(<String>[v.absolute.path, "Auram"]),
        ),
      )
      .then((v) async {
        info("Initializing pack instance");
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
        success("Pack instance initialized");
      });

  void dispose() {
    progressStream.close();
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
    info("Opening launcher data folder: $folderPath");
    await Process.start(
      command.$1,
      command.$2,
      mode: ProcessStartMode.detached,
    );
    success("Open data folder command sent");
  }

  Future<void> forceReinstall() async {
    warn("Force reinstall requested");
    progressStream.add(("Force Reinstall", -1));
    if (await launcherDir.exists()) {
      await launcherDir.delete(recursive: true);
    }
    PackDataUtils.setCurrentVersion("");
    await initialize();
    success("Force reinstall completed");
  }

  Future<void> ensureInstall() async {
    info("Ensuring full installation");
    await ensureJDK();
    await ensurePack();
    await ensureMinecraftFiles();
    success("Installation ensure completed");
  }

  Future<void> ensureJDK() => PackJdkService.ensureJdk(
    progressStream: progressStream,
    tempDir: tempDir,
    javaDir: javaDir,
    jdkDownload: jdkDownload,
  );

  Future<void> ensurePack() => PackRepositoryService.ensurePack(
    progressStream: progressStream,
    tempDir: tempDir,
    gameDir: gameDir,
    minecraftDir: minecraftDir,
  );

  Future<void> ensureMinecraftFiles() =>
      PackRuntimeService.ensureMinecraftFiles(
        progressStream: progressStream,
        tempDir: tempDir,
        javaDir: javaDir,
        gameDir: gameDir,
        minecraftDir: minecraftDir,
        librariesDir: librariesDir,
        versionsDir: versionsDir,
        assetsDir: assetsDir,
        platform: currentPlatform,
        arch: currentArch,
      );
}
