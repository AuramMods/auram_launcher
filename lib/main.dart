import 'dart:io';
import 'dart:math';

import 'package:arcane/arcane.dart';
import 'package:auram_launcher/pack.dart';
import 'package:flutter/scheduler.dart';
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
      themeMode: ThemeMode.dark,
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

  Future<void> simulateLoading() async {
    while (true) {
      for (var i = 0; i <= 100; i++) {
        await Future.delayed(
          Duration(milliseconds: (500 * Random().nextDouble()).round()),
        );
        packInstance.progressStream.add(("Loading... $i%", i / 100));
      }

      packInstance.progressStream.add(("Launching", -1));
      await Future.delayed(Duration(seconds: 3));
    }
  }

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
    gutter: false,
    overrideBackgroundColor: Colors.black,
    child: Container(
      color: Colors.black,
      child: Center(
        child: packInstance.progressStream.buildNullable(
          (p) => p == null
              ? PrimaryButton(onPressed: launch, child: Text("Play"))
              : Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Center(
                      child: ProgressRing(value: p.$2 == -1 ? null : p.$2),
                    ),
                    Center(child: Text(p.$1)),
                  ],
                ),
        ),
      ),
    ),
  );
}

class ProgressRing extends StatefulWidget {
  final double? value;
  final double chase;
  final double snapEpsilon;
  final Curve chaseCurve;
  const ProgressRing({
    super.key,
    required this.value,
    this.chase = 0.1,
    this.snapEpsilon = 0.0005,
    this.chaseCurve = Curves.easeOutCubic,
  });

  @override
  State<ProgressRing> createState() => _ProgressRingState();
}

class _ProgressRingState extends State<ProgressRing>
    with SingleTickerProviderStateMixin {
  double v = 0;
  double t = 0;
  late Ticker ticker;

  double _chaseTo({
    required double current,
    required double target,
    required double normalizer,
  }) {
    double delta = target - current;
    double distance = delta.abs();
    if (distance <= widget.snapEpsilon) return target;
    double scale = normalizer <= 0 ? 1 : normalizer;
    double normalized = (distance / scale).clamp(0.0, 1.0).toDouble();
    double curved = widget.chaseCurve.transform(normalized);
    double step = (distance * widget.chase * curved).clamp(
      widget.snapEpsilon,
      distance,
    );
    return current + (delta < 0 ? -step : step);
  }

  @override
  void initState() {
    super.initState();
    ticker = createTicker((dur) {
      setState(() {
        double targetValue = widget.value ?? -1;
        if (targetValue > 1) targetValue = 1;
        if (targetValue < -1) targetValue = -1;
        double targetThickness = targetValue < 0 ? 10 : (40 * targetValue);
        v = _chaseTo(current: v, target: targetValue, normalizer: 2);
        t = _chaseTo(current: t, target: targetThickness, normalizer: 40);
      });
    });
    ticker.start();
  }

  @override
  void dispose() {
    ticker.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Stack(
    alignment: Alignment.center,
    children: [
      if (true)
        CascadeEffect.prism(
          color: Colors.black,
          repeats: 12,
          shimmer: 10 * v.abs(),
          thickness: 0.1 + (0.7 * v.abs()),
          size: 600,
        ),
    ],
  );
}
