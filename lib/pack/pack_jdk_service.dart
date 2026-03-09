import 'dart:io';

import 'package:arcane/arcane.dart';
import 'package:auram_launcher/pack/pack_file_utils.dart';
import 'package:auram_launcher/pack/pack_install_utils.dart';
import 'package:auram_launcher/pack/pack_network_io.dart';
import 'package:fast_log/fast_log.dart';

class PackJdkService {
  const PackJdkService._();

  static Future<void> ensureJdk({
    required BehaviorSubject<(String, double)?> progressStream,
    required Directory tempDir,
    required Directory javaDir,
    required String jdkDownload,
  }) async {
    info("Ensuring portable JDK at ${javaDir.path}");
    if (await javaDir.exists()) {
      success("Portable JDK already present: ${javaDir.path}");
      return;
    }
    String downloadUrl = jdkDownload;
    Uri downloadUri = Uri.parse(downloadUrl);
    File tempZip = File(
      "${tempDir.absolute.path}${Platform.pathSeparator}jdk.zip",
    );
    Directory extractDir = Directory(
      "${tempDir.absolute.path}${Platform.pathSeparator}jdk_extract",
    );

    if (await tempZip.exists()) {
      verbose("Removing previous temp JDK archive: ${tempZip.path}");
      await tempZip.delete();
    }
    if (await extractDir.exists()) {
      verbose("Removing previous JDK extract dir: ${extractDir.path}");
      await extractDir.delete(recursive: true);
    }
    await extractDir.create(recursive: true);

    verbose("Downloading JDK from $downloadUrl");
    await PackNetworkIo.downloadFile(
      progressStream: progressStream,
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
    await PackInstallUtils.installExtractedDirectory(
      extractDir: extractDir,
      installDir: javaDir,
    );

    if (await tempZip.exists()) {
      await tempZip.delete();
    }
    if (await extractDir.exists()) {
      await extractDir.delete(recursive: true);
    }

    success("Portable JDK ready: ${javaDir.absolute.path}");
  }
}
