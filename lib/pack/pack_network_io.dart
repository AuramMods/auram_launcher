import 'dart:convert';
import 'dart:io';
import 'dart:async';

import 'package:arcane/arcane.dart';
import 'package:fast_log/fast_log.dart';

class PackNetworkIo {
  const PackNetworkIo._();

  static Future<void> downloadFile({
    required BehaviorSubject<(String, double)?> progressStream,
    required Uri uri,
    required File target,
    required String progressLabel,
    required String itemName,
    int? expectedTotalBytes,
    int? assumedTotalBytes,
    bool emitByteProgress = true,
    int maxAttempts = 4,
    int retryBaseDelayMs = 300,
  }) async {
    verbose("Download requested: $itemName from $uri -> ${target.path}");
    if (await target.exists()) {
      if (expectedTotalBytes == null || expectedTotalBytes <= 0) return;
      int existingLength = await target.length();
      if (existingLength == expectedTotalBytes) {
        success("Download skipped (already up to date): ${target.path}");
        return;
      }
      await target.delete();
    }

    Directory parent = target.parent;
    if (!await parent.exists()) {
      await parent.create(recursive: true);
    }

    if (maxAttempts < 1) {
      maxAttempts = 1;
    }
    if (retryBaseDelayMs < 50) {
      retryBaseDelayMs = 50;
    }

    for (int attempt = 1; attempt <= maxAttempts; attempt++) {
      HttpClient client = HttpClient();
      client.connectionTimeout = Duration(seconds: 30);
      try {
        if (attempt > 1) {
          warn("Retrying download ($attempt/$maxAttempts): $itemName");
        }
        HttpClientRequest request = await client.getUrl(uri);
        HttpClientResponse response = await request.close();
        int statusCode = response.statusCode;
        if (statusCode != HttpStatus.ok) {
          if (_isRetryableStatus(statusCode)) {
            throw _RetryableHttpStatusException(
              statusCode: statusCode,
              uri: uri,
            );
          }
          error("Download failed: $itemName status $statusCode at $uri");
          throw HttpException(
            "Failed to download $itemName (status $statusCode)",
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
          success("Download completed: $itemName -> ${target.path}");
          return;
        } finally {
          await sink.close();
        }
      } on Object catch (error) {
        if (await target.exists()) {
          await target.delete();
        }

        bool canRetry =
            attempt < maxAttempts && _isRetryableDownloadError(error);
        if (!canRetry) rethrow;

        int exponent = attempt - 1;
        if (exponent > 8) exponent = 8;
        int delayMs = retryBaseDelayMs * (1 << exponent);
        warn(
          "Transient download failure for $itemName ($error). "
          "Retrying in ${delayMs}ms",
        );
        await Future.delayed(Duration(milliseconds: delayMs));
      } finally {
        client.close(force: true);
      }
    }
  }

  static Future<Map<String, dynamic>> readJsonFromUri({
    required Uri uri,
  }) async {
    verbose("Reading JSON object from URI: $uri");
    HttpClient client = HttpClient();
    client.connectionTimeout = Duration(seconds: 30);
    try {
      HttpClientRequest request = await client.getUrl(uri);
      HttpClientResponse response = await request.close();
      if (response.statusCode != HttpStatus.ok) {
        error("JSON object request failed status ${response.statusCode}: $uri");
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
      if (decoded is Map<String, dynamic>) {
        success("Read JSON object from URI: $uri");
        return decoded;
      }
      if (decoded is Map) {
        success("Read JSON object from URI: $uri");
        return decoded.cast<String, dynamic>();
      }
      error("JSON object parse failed for URI: $uri");
      throw FormatException("Expected JSON object at $uri");
    } finally {
      client.close(force: true);
    }
  }

  static Future<List<dynamic>> readJsonListFromUri({required Uri uri}) async {
    verbose("Reading JSON array from URI: $uri");
    HttpClient client = HttpClient();
    client.connectionTimeout = Duration(seconds: 30);
    try {
      HttpClientRequest request = await client.getUrl(uri);
      HttpClientResponse response = await request.close();
      if (response.statusCode != HttpStatus.ok) {
        error("JSON array request failed status ${response.statusCode}: $uri");
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
      if (decoded is List) {
        success("Read JSON array from URI: $uri");
        return decoded;
      }
      error("JSON array parse failed for URI: $uri");
      throw FormatException("Expected JSON array at $uri");
    } finally {
      client.close(force: true);
    }
  }

  static Future<Map<String, dynamic>> readJsonFile({required File file}) async {
    verbose("Reading JSON file: ${file.path}");
    String content = await file.readAsString();
    dynamic decoded = jsonDecode(content);
    if (decoded is Map<String, dynamic>) {
      success("Read JSON file: ${file.path}");
      return decoded;
    }
    if (decoded is Map) {
      success("Read JSON file: ${file.path}");
      return decoded.cast<String, dynamic>();
    }
    error("JSON file parse failed: ${file.path}");
    throw FormatException("Expected JSON object in ${file.path}");
  }

  static bool _isRetryableDownloadError(Object error) {
    if (error is SocketException) return true;
    if (error is TimeoutException) return true;
    if (error is HandshakeException) return true;
    if (error is _RetryableHttpStatusException) return true;
    return false;
  }

  static bool _isRetryableStatus(int statusCode) {
    if (statusCode == 408 || statusCode == 429) return true;
    return statusCode >= 500 && statusCode <= 599;
  }
}

class _RetryableHttpStatusException implements Exception {
  final int statusCode;
  final Uri uri;

  const _RetryableHttpStatusException({
    required this.statusCode,
    required this.uri,
  });

  @override
  String toString() => "Retryable HTTP status $statusCode at $uri";
}
