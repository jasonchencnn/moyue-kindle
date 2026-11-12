cmake_minimum_required(VERSION 3.16)
project(koreader_crengine C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

if(NOT DEFINED KO_CRE_DIR)
    set(KO_CRE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../vendor/koreader-crengine")
endif()

include_directories(
    "${CMAKE_CURRENT_SOURCE_DIR}"
    "${KO_CRE_DIR}/crengine/include"
    "${KO_CRE_DIR}/thirdparty/xxhash"
    ${EXTRA_INCLUDES}
)

set(SRCS
    ${KO_CRE_DIR}/crengine/qimagescale/qimagescale.cpp
    ${KO_CRE_DIR}/crengine/src/chmfmt.cpp
    ${KO_CRE_DIR}/crengine/src/cp_stats.cpp
    ${KO_CRE_DIR}/crengine/src/cri18n.cpp
    ${KO_CRE_DIR}/crengine/src/crtxtenc.cpp
    ${KO_CRE_DIR}/crengine/src/docxfmt.cpp
    ${KO_CRE_DIR}/crengine/src/epubfmt.cpp
    ${KO_CRE_DIR}/crengine/src/fb3fmt.cpp
    ${KO_CRE_DIR}/crengine/src/hist.cpp
    ${KO_CRE_DIR}/crengine/src/hyphman.cpp
    ${KO_CRE_DIR}/crengine/src/lstridmap.cpp
    ${KO_CRE_DIR}/crengine/src/lvdocview.cpp
    ${KO_CRE_DIR}/crengine/src/lvdrawbuf.cpp
    ${KO_CRE_DIR}/crengine/src/lvfnt.cpp
    ${KO_CRE_DIR}/crengine/src/lvfntman.cpp
    ${KO_CRE_DIR}/crengine/src/lvimg.cpp
    ${KO_CRE_DIR}/crengine/src/lvmemman.cpp
    ${KO_CRE_DIR}/crengine/src/lvopc.cpp
    ${KO_CRE_DIR}/crengine/src/lvpagesplitter.cpp
    ${KO_CRE_DIR}/crengine/src/lvrend.cpp
    ${KO_CRE_DIR}/crengine/src/lvstream.cpp
    ${KO_CRE_DIR}/crengine/src/lvstring.cpp
    ${KO_CRE_DIR}/crengine/src/lvstsheet.cpp
    ${KO_CRE_DIR}/crengine/src/lvstyles.cpp
    ${KO_CRE_DIR}/crengine/src/lvtextfm.cpp
    ${KO_CRE_DIR}/crengine/src/lvtinydom.cpp
    ${KO_CRE_DIR}/crengine/src/lvxml.cpp
    ${KO_CRE_DIR}/crengine/src/mathml.cpp
    ${KO_CRE_DIR}/crengine/src/mdfmt.cpp
    ${KO_CRE_DIR}/crengine/src/odtfmt.cpp
    ${KO_CRE_DIR}/crengine/src/odxutil.cpp
    ${KO_CRE_DIR}/crengine/src/pdbfmt.cpp
    ${KO_CRE_DIR}/crengine/src/props.cpp
    ${KO_CRE_DIR}/crengine/src/renderutil.cpp
    ${KO_CRE_DIR}/crengine/src/rtfimp.cpp
    ${KO_CRE_DIR}/crengine/src/textlang.cpp
    ${KO_CRE_DIR}/crengine/src/txtselector.cpp
    ${KO_CRE_DIR}/crengine/src/wordfmt.cpp
    ${KO_CRE_DIR}/thirdparty/xxhash/xxhash.c
)

add_library(koreader_crengine STATIC ${SRCS})
target_compile_options(koreader_crengine PRIVATE -include "${CMAKE_CURRENT_SOURCE_DIR}/crsetup.h" -Wno-everything)

install(TARGETS koreader_crengine ARCHIVE DESTINATION lib)
install(DIRECTORY "${KO_CRE_DIR}/crengine/include/" DESTINATION include)
install(FILES "${CMAKE_CURRENT_SOURCE_DIR}/crsetup.h" DESTINATION include)
