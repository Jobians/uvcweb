Python reference implementation of the RTP/JPEG + L16 + RTCP + RTSP logic.
It was tested against ffmpeg (TCP and UDP transport; 4:2:2, 4:2:0 and restart-marker JPEGs;
mono 96 kHz and stereo 48 kHz audio). The Rust code in src/ is a port of it and
src/golden_tests.rs holds vectors generated from it.
Run it (needs Pillow): python3 rtsp_server.py 8554   then   ffplay rtsp://127.0.0.1:8554/live
