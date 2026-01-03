#ifdef MA_ENABLE_VORBIS
#define STB_VORBIS_HEADER_ONLY
#include "./miniaudio/extras/stb_vorbis.c"
#endif

#include "./miniaudio/miniaudio.h"

#ifdef MA_ENABLE_VORBIS
#undef STB_VORBIS_HEADER_ONLY
#include "./miniaudio/extras/stb_vorbis.c"
#endif
