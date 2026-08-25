// MIT License
// Copyright (c) 2019-2021 bloc97
// Source: Anime4K v4.0, glsl/Restore/Anime4K_Clamp_Highlights.glsl

//!DESC Anime4K-v4.0-De-Ring-Compute-Statistics
//!HOOK MAIN
//!BIND HOOKED
//!SAVE STATSMAX
//!COMPONENTS 1

#define KERNELSIZE 5
#define KERNELHALFSIZE 2

float get_luma(vec4 rgba) {
    return dot(vec4(0.299, 0.587, 0.114, 0.0), rgba);
}

vec4 hook() {
    float gmax = 0.0;
    for (int i = 0; i < KERNELSIZE; i++) {
        float g = get_luma(MAIN_texOff(vec2(i - KERNELHALFSIZE, 0)));
        gmax = max(g, gmax);
    }
    return vec4(gmax, 0.0, 0.0, 0.0);
}

//!DESC Anime4K-v4.0-De-Ring-Compute-Statistics
//!HOOK MAIN
//!BIND HOOKED
//!BIND STATSMAX
//!SAVE STATSMAX
//!COMPONENTS 1

#define KERNELSIZE 5
#define KERNELHALFSIZE 2

vec4 hook() {
    float gmax = 0.0;
    for (int i = 0; i < KERNELSIZE; i++) {
        float g = STATSMAX_texOff(vec2(0, i - KERNELHALFSIZE)).x;
        gmax = max(g, gmax);
    }
    return vec4(gmax, 0.0, 0.0, 0.0);
}

//!DESC Anime4K-v4.0-De-Ring-Clamp
//!HOOK PREKERNEL
//!BIND HOOKED
//!BIND STATSMAX

float get_luma(vec4 rgba) {
    return dot(vec4(0.299, 0.587, 0.114, 0.0), rgba);
}

vec4 hook() {
    float current_luma = get_luma(HOOKED_tex(HOOKED_pos));
    float new_luma = min(current_luma, STATSMAX_tex(HOOKED_pos).x);
    return HOOKED_tex(HOOKED_pos) - (current_luma - new_luma);
}
