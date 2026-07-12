// builds reliable_config_t inside C, so the Rust test crate never mirrors the
// struct layout and cannot get it wrong

#include "reliable.h"

typedef void (*wire_compat_transmit_fn)( void *, uint64_t, uint16_t, uint8_t *, int );
typedef int (*wire_compat_process_fn)( void *, uint64_t, uint16_t, uint8_t *, int );

struct reliable_endpoint_t * wire_compat_endpoint_create( int fragment_above,
                                                          uint64_t id,
                                                          void * context,
                                                          wire_compat_transmit_fn transmit,
                                                          wire_compat_process_fn process )
{
    struct reliable_config_t config;
    reliable_default_config( &config );
    config.fragment_above = fragment_above;
    config.id = id;
    config.context = context;
    config.transmit_packet_function = transmit;
    config.process_packet_function = process;
    return reliable_endpoint_create( &config, 100.0 );
}
