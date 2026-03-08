import 'dart:io';

import 'package:auram_launcher/pack/pack_path_utils.dart';

class PackFileUtils {
  const PackFileUtils._();

  static Future<void> copyEntity({
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
        await copyEntity(
          source: child,
          destination:
              "$destination${Platform.pathSeparator}${PackPathUtils.basename(child.path)}",
        );
      }
      return;
    }

    if (source is Link) {
      String target = await source.target();
      await Link(destination).create(target);
    }
  }

  static Future<void> extractArchive({
    required File tempZip,
    required Directory extractDir,
  }) async {
    if (Platform.isWindows) {
      String command =
          "Expand-Archive -LiteralPath ${psQuote(tempZip.path)} -DestinationPath ${psQuote(extractDir.path)} -Force";
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

  static String psQuote(String input) => "'${input.replaceAll("'", "''")}'";
}
