import 'dart:io';

import 'package:arcane/arcane.dart';
import 'package:auram_launcher/pack.dart';
import 'package:flutter/services.dart';
import 'package:microshaft/microshaft.dart';
import 'package:microshaft/src/model/shafted.dart';
import 'package:url_launcher/url_launcher.dart';

void main() => runApp("auram_launcher", AuramLauncher());

class AuramLauncher extends StatelessWidget {
  const AuramLauncher({super.key});

  @override
  Widget build(BuildContext context) => ArcaneApp(
    home: MainScreen(),
    title: "Auram",
    theme: ArcaneTheme(
      scheme: ContrastedColorScheme.fromScheme(ColorSchemes.violet),
    ),
  );
}

class MainScreen extends StatefulWidget {
  const MainScreen({super.key});

  @override
  State<MainScreen> createState() => _MainScreenState();
}

class _MainScreenState extends State<MainScreen> {
  late PackInstance packInstance;
  late Future<void> initFuture;

  @override
  void initState() {
    super.initState();
    packInstance = PackInstance();
    initFuture = packInstance.initialize();
  }

  Future<Shafted> getShafted() =>
      MicroshaftClient(storage: FileStorage.load("tokens.dat"))
          .authenticate((url, code) async {
            warningAnnounce("CODE: $code");
            await Clipboard.setData(ClipboardData(text: code));
            TextToast("Copied code '$code' to clipboard!").open(context);
            launchUrl(Uri.parse(url));
          })
          .thenRun((value) {
            successAnnounce("Logged in as ${value.profileName}");
          });

  Future<void> launch() async {
    await initFuture;
    packInstance.progressStream.add(("Launching", -1));
    verboseAnnounce("Launching Auram");
    Shafted auth = await getShafted();
    await packInstance.launch(auth);
    exit(0);
  }

  @override
  void dispose() {
    packInstance.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Screen(
    child: Center(
      child: packInstance.progressStream.buildNullable(
        (p) => p == null
            ? PrimaryButton(onPressed: launch, child: Text("Play"))
            : Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  CircularProgressIndicator(
                    value: p.$2 == -1 ? null : p.$2,
                    size: 100,
                  ),
                  Gap(16),
                  Text(p.$1),
                ],
              ),
      ),
    ),
  );
}
